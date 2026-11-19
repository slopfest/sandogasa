// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `timebook stale`: which recurring meetings in Fedora Calendar still
//! take place, from the minutes meetbot keeps, and how active their
//! owners are.
//!
//! meetbot is searched by topic, so each series is looked for under its
//! calendar's name and its title as a topic (`fesco`,
//! `centos-hyperscale-sig`), and an occurrence counts as held when
//! minutes in its room start within an hour of it.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveDateTime, Timelike, Utc};
use sandogasa_fedocal::{Client, Meeting, MeetingQuery};
use serde::Serialize;

/// How far from an occurrence's start minutes may begin and still count
/// for it.
const SLACK_MINUTES: i64 = 60;

/// fedocal's default end for a series with none, until 2026-05-29
/// (fedocal 98fdece): a series created without an end stopped there.
pub const OLD_DEFAULT_END: &str = "2025-12-31";

/// A recurring calendar entry, with its occurrences in a period.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Series {
    pub meeting_id: u64,
    pub name: String,
    pub calendar: String,
    pub location: String,
    /// The shared room its location names, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub room: Option<String>,
    pub owners: Vec<String>,
    pub occurrences: Vec<DateTime<Utc>>,
}

/// One meeting meetbot keeps minutes of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Minutes {
    pub channel: String,
    pub topic: String,
    pub at: NaiveDateTime,
    pub url: String,
}

/// How a series fared against the minutes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// No minutes for any occurrence.
    NotHeld,
    /// Minutes for fewer than half.
    RarelyHeld,
    /// A video call, which meetbot does not log.
    VideoCall,
    /// Minutes for half or more.
    Held,
}

/// How a series fared against the minutes, as [`judge`] finds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Judgment {
    pub verdict: Verdict,
    pub held: usize,
    pub last_minutes: Option<String>,
    /// Rooms minutes were found in instead of the one it is booked in.
    pub elsewhere: Vec<String>,
}

/// The findings for one series.
#[derive(Debug, Clone, Serialize)]
pub struct SeriesReport {
    #[serde(flatten)]
    pub series: Series,
    pub verdict: Verdict,
    /// Occurrences with minutes.
    pub held: usize,
    /// The newest minutes found for it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_minutes: Option<String>,
    /// Rooms its minutes were found in instead of the one it is booked
    /// in: the booking may be out of date.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub elsewhere: Vec<String>,
    /// The topics meetbot was searched for.
    pub searched: Vec<String>,
    /// Per owner, what Discourse says of them.
    pub owners_seen: BTreeMap<String, OnDiscourse>,
}

/// What Discourse says of an owner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OnDiscourse {
    /// The day it last saw them.
    Seen(String),
    /// No such user.
    NotFound,
    /// The lookup failed (a rate limit, a server error), and why.
    Failed(String),
}

/// A series that has occurrences up to fedocal's old default end and
/// none after, with any minutes since under its topics.
#[derive(Debug, Clone, Serialize)]
pub struct Dropped {
    #[serde(flatten)]
    pub series: Series,
    pub searched: Vec<String>,
    /// The newest minutes since the old default end, at its time: when
    /// recent, a sign it is still held and its entry needs a new end.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minutes_since: Option<MinutesRef>,
}

/// Minutes, by day and page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MinutesRef {
    pub date: NaiveDate,
    pub url: String,
}

/// Where `stale` looks besides Fedora Calendar: meetbot for minutes and
/// Discourse for owners' activity, production unless a test says
/// otherwise.
#[derive(Debug, Clone)]
pub struct Sources {
    pub meetbot: String,
    pub discourse: String,
    /// Keep meetbot's answers for a day under timebook's cache.
    pub cache: bool,
}

/// The recurring series among `meetings` (two or more occurrences), each
/// once, its occurrences in order.
pub fn series(meetings: &[Meeting]) -> Vec<Series> {
    let mut by_id: BTreeMap<u64, Series> = BTreeMap::new();
    for m in meetings {
        let Some(start) = m.starts_at() else {
            continue;
        };
        let s = by_id.entry(m.meeting_id).or_insert_with(|| Series {
            meeting_id: m.meeting_id,
            name: m.meeting_name.clone(),
            calendar: m.calendar_name.clone(),
            location: m.meeting_location.clone(),
            room: m.room().map(|r| r.name),
            owners: m.meeting_manager.clone(),
            occurrences: Vec::new(),
        });
        s.occurrences.push(start);
    }
    by_id
        .into_values()
        .map(|mut s| {
            s.occurrences.sort();
            s.occurrences.dedup();
            s
        })
        .filter(|s| s.occurrences.len() >= 2)
        .collect()
}

/// The topics to search meetbot for: the calendar's name, and the title
/// as a topic, with and without a trailing "meeting".
pub fn topic_terms(s: &Series) -> Vec<String> {
    let mut terms: Vec<String> = Vec::new();
    let mut add = |t: String| {
        if t.len() >= 3 && !terms.contains(&t) {
            terms.push(t);
        }
    };
    add(slug(&s.calendar));
    let title = slug(&s.name);
    // Many teams' topics carry a `fedora-` the title leaves out
    // (`fedora-eln-sig` for "ELN SIG").
    let prefixed = if title.starts_with("fedora-") {
        None
    } else {
        Some(format!("fedora-{title}"))
    };
    for t in std::iter::once(title).chain(prefixed) {
        add(t.clone());
        if let Some(short) = t.strip_suffix("-meeting") {
            add(short.to_string());
        }
    }
    terms
}

/// Whether a minutes' `topic` carries every word of one of `s`'s topic
/// terms: `fedora-coreos-meeting` carries `coreos`, `fedora-ai-ml-sig`
/// does not carry `sigs`.
pub fn topic_fits(s: &Series, topic: &str) -> bool {
    let words: BTreeSet<String> = slug(topic).split('-').map(String::from).collect();
    topic_terms(s)
        .iter()
        .any(|t| t.split('-').all(|w| words.contains(w)))
}

/// The room a meetbot channel is, in fedocal's naming:
/// `meeting-1_matrix_fedoraproject-org` and the IRC `fedora-meeting-1`
/// are both `meeting-1`.
pub fn channel_room(channel: &str) -> Option<String> {
    let name = channel
        .strip_suffix("_matrix_fedoraproject-org")
        .or_else(|| channel.strip_prefix("fedora-"))?;
    sandogasa_fedocal::room::Room::parse(name).map(|r| r.name)
}

/// Whether `location` is a video call, which meetbot does not log.
pub fn is_video_call(location: &str) -> bool {
    let l = location.to_ascii_lowercase();
    [
        "meet.google.",
        "zoom.us",
        "meet.jit.si",
        "jitsi",
        "bluejeans",
        "/meet/",
        "meet.",
    ]
    .iter()
    .any(|v| l.contains(v))
}

/// How `s` fared against `minutes`: the occurrences with minutes
/// starting within an hour, preferably in its room; the newest such
/// minutes; and the rooms minutes were found in instead of the one it is
/// booked in, a sign the booking is out of date.
pub fn judge(s: &Series, minutes: &[Minutes]) -> Judgment {
    let near = |occ: &DateTime<Utc>, m: &Minutes| {
        (m.at.and_utc() - *occ).num_minutes().abs() <= SLACK_MINUTES
    };
    // Without a room, minutes in any channel may be its own, so their
    // topic must carry one of its terms whole: meetbot's search finds any
    // topic containing a term, and `sigs` is in many.
    let in_room = |m: &Minutes| match &s.room {
        Some(room) => channel_room(&m.channel).as_deref() == Some(room.as_str()),
        None => topic_fits(s, &m.topic),
    };
    let mut held = 0;
    let mut last: Option<&Minutes> = None;
    let mut elsewhere = BTreeSet::new();
    for occ in &s.occurrences {
        let close: Vec<&Minutes> = minutes.iter().filter(|m| near(occ, m)).collect();
        // Minutes in another room count, as a sign the booking is out of
        // date, only for a series booked in a room: without one, only
        // minutes whose topic fits are its own.
        let found = close
            .iter()
            .filter(|m| in_room(m))
            .max_by_key(|m| m.at)
            .or_else(|| {
                s.room
                    .is_some()
                    .then(|| close.iter().max_by_key(|m| m.at))
                    .flatten()
            });
        if let Some(m) = found {
            held += 1;
            if !in_room(m) {
                elsewhere.insert(channel_room(&m.channel).unwrap_or_else(|| m.channel.clone()));
            }
            if last.is_none_or(|l| m.at > l.at) {
                last = Some(m);
            }
        }
    }
    let verdict = if held * 2 >= s.occurrences.len() {
        Verdict::Held
    } else if held > 0 {
        Verdict::RarelyHeld
    } else if is_video_call(&s.location) {
        Verdict::VideoCall
    } else {
        Verdict::NotHeld
    };
    Judgment {
        verdict,
        held,
        last_minutes: last.map(|m| m.url.clone()),
        elsewhere: elsewhere.into_iter().collect(),
    }
}

/// The series that occur on or before `end` and not after it, among
/// those in `before` (occurrences up to `end`) and `after` (occurrences
/// since).
pub fn dropped_at(before: &[Series], after: &[Meeting], end: NaiveDate) -> Vec<Series> {
    // A day's slack: dates are UTC, and an evening meeting in the west on
    // `end` falls on the next day there.
    let edge = end + Duration::days(1);
    let later: BTreeSet<u64> = after
        .iter()
        .filter(|m| m.meeting_date > edge)
        .map(|m| m.meeting_id)
        .collect();
    before
        .iter()
        .filter(|s| !later.contains(&s.meeting_id))
        .filter(|s| s.occurrences.last().is_some_and(|o| o.date_naive() <= edge))
        .filter(|s| {
            // Ended in the last weeks before it: a series that stopped
            // long before ended for another reason.
            s.occurrences
                .last()
                .is_some_and(|o| o.date_naive() > end - Duration::days(28))
        })
        .cloned()
        .collect()
}

/// The newest of `minutes` after `since` that fall when `s` meets: on
/// the weekday of one of its occurrences and within an hour and a half
/// of its time of day (a daylight-saving change moves the UTC time by an
/// hour).
pub fn newest_since(s: &Series, minutes: &[Minutes], since: NaiveDate) -> Option<MinutesRef> {
    let fits = |m: &Minutes| {
        let at = m.at.and_utc();
        s.occurrences.iter().any(|o| week_distance(*o, at) <= 90)
    };
    minutes
        .iter()
        .filter(|m| m.at.date() > since && fits(m))
        .max_by_key(|m| m.at)
        .map(|m| MinutesRef {
            date: m.at.date(),
            url: m.url.clone(),
        })
}

/// Whether `old` is replaced by one of `current`: a series of the same
/// calendar whose title is the same once `fedora-` and `meeting` are set
/// aside, or that meets on the same weekday within an hour of the same
/// time.
pub fn replaced(old: &Series, current: &[Series]) -> bool {
    // The title's words without `fedora` and `meeting`.
    let words = |name: &str| -> BTreeSet<String> {
        slug(name)
            .split('-')
            .filter(|w| !w.is_empty() && *w != "fedora" && *w != "meeting")
            .map(String::from)
            .collect()
    };
    let (old_words, old_at) = (words(&old.name), old.occurrences.last());
    current.iter().any(|c| {
        if c.calendar != old.calendar || c.meeting_id == old.meeting_id {
            return false;
        }
        // The same words, or the fewer (two or more) all in the other:
        // one word such as `sig` is in too many titles.
        let c_words = words(&c.name);
        let (fewer, more) = if c_words.len() <= old_words.len() {
            (&c_words, &old_words)
        } else {
            (&old_words, &c_words)
        };
        let same_title =
            !fewer.is_empty() && (fewer == more || (fewer.len() >= 2 && fewer.is_subset(more)));
        let same_slot = match (old_at, c.occurrences.last()) {
            (Some(o), Some(n)) => week_distance(*o, *n) <= 60,
            _ => false,
        };
        same_title || same_slot
    })
}

/// Everything `timebook stale` reports: the recurring series of the
/// `weeks` up to `now`, judged against meetbot, and the series that
/// stopped at fedocal's old default end. Occurrences that started less
/// than two hours before `now` are left out: meetbot cannot have their
/// minutes yet.
pub async fn report(
    client: &Client,
    sources: &Sources,
    now: DateTime<Utc>,
    weeks: i64,
    refresh: bool,
) -> Result<(Vec<SeriesReport>, Vec<Dropped>), String> {
    let today = now.date_naive();
    let from = today - Duration::weeks(weeks);
    let read = |from: NaiveDate, to: NaiveDate| async move {
        eprintln!("Reading Fedora Calendar from {from} to {to}...");
        client
            .meetings(&MeetingQuery::default().between(from, to))
            .await
            .map_err(|e| e.to_string())
    };
    let recurring = up_to(series(&read(from, today).await?), now - Duration::hours(2));
    let end = NaiveDate::parse_from_str(OLD_DEFAULT_END, "%Y-%m-%d").expect("a date");
    // A day either side: an evening meeting in the west of 2025-12-31
    // is on 2026-01-01 in UTC.
    let before = series(&read(end - Duration::days(28), end + Duration::days(1)).await?);
    let after = read(end + Duration::days(1), end + Duration::days(28)).await?;
    let dropped: Vec<Series> = dropped_at(&before, &after, end)
        .into_iter()
        .filter(|s| !recurring.iter().any(|r| r.meeting_id == s.meeting_id))
        .filter(|s| !replaced(s, &recurring))
        .collect();

    let terms: BTreeSet<String> = recurring
        .iter()
        .chain(&dropped)
        .flat_map(topic_terms)
        .collect();
    eprintln!("Searching meetbot for {} topic(s)...", terms.len());
    let found = search_meetbot(terms, sources, refresh).await;
    let minutes_for = |s: &Series| -> (Vec<String>, Vec<Minutes>) {
        let searched = topic_terms(s);
        let mut all: Vec<Minutes> = searched
            .iter()
            .flat_map(|t| found.get(t).cloned().unwrap_or_default())
            .collect();
        all.sort_by(|a, b| a.url.cmp(&b.url));
        all.dedup_by(|a, b| a.url == b.url);
        (searched, all)
    };

    let mut reports: Vec<SeriesReport> = recurring
        .into_iter()
        .map(|s| {
            let (searched, minutes) = minutes_for(&s);
            let j = judge(&s, &minutes);
            SeriesReport {
                series: s,
                verdict: j.verdict,
                held: j.held,
                last_minutes: j.last_minutes,
                elsewhere: j.elsewhere,
                searched,
                owners_seen: BTreeMap::new(),
            }
        })
        .collect();
    // Only the owners of series that look stale are looked up.
    let owners: BTreeSet<String> = reports
        .iter()
        .filter(|r| matches!(r.verdict, Verdict::NotHeld | Verdict::RarelyHeld))
        .flat_map(|r| r.series.owners.clone())
        .collect();
    if !owners.is_empty() {
        eprintln!("Looking up {} owner(s) on Discourse...", owners.len());
    }
    let seen = last_seen(owners, &sources.discourse).await;
    for r in &mut reports {
        if matches!(r.verdict, Verdict::NotHeld | Verdict::RarelyHeld) {
            r.owners_seen = r
                .series
                .owners
                .iter()
                .map(|o| {
                    let state = seen
                        .get(o)
                        .cloned()
                        .unwrap_or_else(|| OnDiscourse::Failed("not looked up".into()));
                    (o.clone(), state)
                })
                .collect();
        }
    }
    reports.sort_by(|a, b| {
        (a.verdict, &a.series.calendar, &a.series.name).cmp(&(
            b.verdict,
            &b.series.calendar,
            &b.series.name,
        ))
    });
    let dropped = dropped
        .into_iter()
        .map(|s| {
            let (searched, minutes) = minutes_for(&s);
            Dropped {
                minutes_since: newest_since(&s, &minutes, end),
                series: s,
                searched,
            }
        })
        .collect();
    Ok((reports, dropped))
}

/// The report as text, grouped by verdict.
pub fn describe(
    reports: &[SeriesReport],
    dropped: &[Dropped],
    from: NaiveDate,
    to: NaiveDate,
) -> String {
    let mut out = format!("Recurring meetings in Fedora Calendar from {from} to {to}:\n");
    let groups = [
        (Verdict::NotHeld, "No minutes found"),
        (Verdict::RarelyHeld, "Rarely held"),
        (
            Verdict::VideoCall,
            "Video calls, which meetbot does not log",
        ),
        (Verdict::Held, "Held"),
    ];
    for (verdict, title) in groups {
        let these: Vec<&SeriesReport> = reports.iter().filter(|r| r.verdict == verdict).collect();
        if these.is_empty() {
            continue;
        }
        out.push_str(&format!("\n{title} ({}):\n", these.len()));
        for r in these {
            out.push_str(&format!("  {}\n", headline(&r.series)));
            match verdict {
                Verdict::Held | Verdict::RarelyHeld => {
                    out.push_str(&format!(
                        "    held {} of {}, last {}\n",
                        r.held,
                        r.series.occurrences.len(),
                        r.last_minutes.as_deref().unwrap_or("-")
                    ));
                    if !r.elsewhere.is_empty() {
                        out.push_str(&format!(
                            "    minutes are in {}, not the room booked\n",
                            r.elsewhere.join(", ")
                        ));
                    }
                }
                Verdict::NotHeld => out.push_str(&format!(
                    "    searched meetbot for: {}\n",
                    r.searched.join(", ")
                )),
                Verdict::VideoCall => {}
            }
            if !r.owners_seen.is_empty() {
                let owners: Vec<String> = r
                    .owners_seen
                    .iter()
                    .map(|(o, seen)| match seen {
                        OnDiscourse::Seen(day) => format!("{o} (seen {day})"),
                        OnDiscourse::NotFound => format!("{o} (not on Discourse)"),
                        OnDiscourse::Failed(_) => format!("{o} (Discourse lookup failed)"),
                    })
                    .collect();
                out.push_str(&format!("    owners: {}\n", owners.join(", ")));
            }
        }
    }
    if !dropped.is_empty() {
        out.push_str(&format!(
            "\nStopped at {OLD_DEFAULT_END}, fedocal's old default end for a series without one ({}):\n",
            dropped.len()
        ));
        for d in dropped {
            out.push_str(&format!("  {}\n", headline(&d.series)));
            out.push_str(&match &d.minutes_since {
                // Minutes in the period: it goes on without an entry.
                Some(m) if m.date >= from => format!(
                    "    still held: minutes on {} ({}); its entry needs a new end\n",
                    m.date, m.url
                ),
                Some(m) => format!(
                    "    last minutes on {} ({}), none in the period\n",
                    m.date, m.url
                ),
                None => format!(
                    "    no minutes since (searched meetbot for: {})\n",
                    d.searched.join(", ")
                ),
            });
        }
    }
    out
}

impl Default for Sources {
    fn default() -> Self {
        Self {
            meetbot: "https://meetbot.fedoraproject.org".into(),
            discourse: "https://discussion.fedoraproject.org".into(),
            cache: true,
        }
    }
}

/// `series` with only the occurrences that started by `cutoff`, those
/// left with none dropped.
fn up_to(series: Vec<Series>, cutoff: DateTime<Utc>) -> Vec<Series> {
    series
        .into_iter()
        .map(|mut s| {
            s.occurrences.retain(|o| *o <= cutoff);
            s
        })
        .filter(|s| !s.occurrences.is_empty())
        .collect()
}

/// How far apart two instants are in the week, in minutes, whatever the
/// weeks: Wednesday 23:30 and Thursday 00:30 are an hour apart.
fn week_distance(a: DateTime<Utc>, b: DateTime<Utc>) -> i64 {
    const WEEK: i64 = 7 * 24 * 60;
    let in_week = |t: DateTime<Utc>| {
        i64::from(t.weekday().num_days_from_monday()) * 24 * 60
            + i64::from(t.time().num_seconds_from_midnight() / 60)
    };
    let d = (in_week(a) - in_week(b)).rem_euclid(WEEK);
    d.min(WEEK - d)
}

/// `text` as a meetbot topic: lowercase, runs of anything else than
/// letters and digits as one `-`.
fn slug(text: &str) -> String {
    let mut out = String::new();
    for c in text.to_lowercase().chars() {
        if c.is_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    out.trim_end_matches('-').to_string()
}

/// A series in a line: title, calendar, when and where it meets.
fn headline(s: &Series) -> String {
    let when = s
        .occurrences
        .last()
        .map(|o| o.format("%a %H:%M UTC").to_string())
        .unwrap_or_default();
    let place = match (&s.room, s.location.trim()) {
        (Some(room), _) => format!("in {room}"),
        (None, "") => "no location".to_string(),
        (None, loc) => loc.to_string(),
    };
    let shared = if s.calendar == "SIGs" {
        " [shared calendar]"
    } else {
        ""
    };
    format!(
        "{} ({}{shared}): {when}, {place}, {} occurrence(s)",
        s.name,
        s.calendar,
        s.occurrences.len()
    )
}

/// The minutes meetbot has under each of `terms`, four searches at a
/// time; a failed search is a warning, and finds nothing.
async fn search_meetbot(
    terms: BTreeSet<String>,
    sources: &Sources,
    refresh: bool,
) -> BTreeMap<String, Vec<Minutes>> {
    let terms: Vec<String> = terms.into_iter().collect();
    let mut out = BTreeMap::new();
    for chunk in terms.chunks(4) {
        let mut set = tokio::task::JoinSet::new();
        for term in chunk {
            let (term, base, cache) = (term.clone(), sources.meetbot.clone(), sources.cache);
            set.spawn_blocking(move || {
                let bot = sandogasa_meetbot::Meetbot::with_base_url(&base);
                let bot = if cache {
                    bot.with_cache("timebook", refresh)
                } else {
                    bot
                };
                let found = bot.search(&term).map_err(|e| e.to_string());
                (term, found)
            });
        }
        while let Some(done) = set.join_next().await {
            let (term, found) = match done {
                Ok(done) => done,
                Err(e) => {
                    eprintln!("warning: a meetbot search stopped: {e}");
                    continue;
                }
            };
            match found {
                Ok(meetings) => {
                    let minutes = meetings
                        .into_iter()
                        .map(|m| Minutes {
                            channel: m.channel,
                            topic: m.topic,
                            at: m.datetime,
                            url: m.summary_url,
                        })
                        .collect();
                    out.insert(term, minutes);
                }
                Err(e) => eprintln!("warning: cannot search meetbot for {term}: {e}"),
            }
        }
    }
    out
}

/// When Discourse last saw each of `users` (`None`: not found or not
/// readable), six at a time.
async fn last_seen(users: BTreeSet<String>, discourse: &str) -> BTreeMap<String, OnDiscourse> {
    let users: Vec<String> = users.into_iter().collect();
    let mut out = BTreeMap::new();
    for chunk in users.chunks(6) {
        let mut set = tokio::task::JoinSet::new();
        for user in chunk {
            let (user, base) = (user.clone(), discourse.to_string());
            set.spawn(async move {
                let client = sandogasa_discourse::DiscourseClient::new(&base);
                // The client answers a 404 with this message; anything else
                // (a rate limit, a server error) says nothing of the user.
                let seen = match client.user(&user).await {
                    Ok(u) => match u.last_seen_at {
                        Some(t) => OnDiscourse::Seen(t.format("%Y-%m-%d").to_string()),
                        None => OnDiscourse::NotFound,
                    },
                    Err(e) if e.to_string().contains("not found on Discourse") => {
                        OnDiscourse::NotFound
                    }
                    Err(e) => OnDiscourse::Failed(e.to_string()),
                };
                (user, seen)
            });
        }
        while let Some(done) = set.join_next().await {
            match done {
                Ok((user, seen)) => {
                    out.insert(user, seen);
                }
                Err(e) => eprintln!("warning: a Discourse lookup stopped: {e}"),
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meeting(id: u64, date: &str, time: &str, cal: &str, name: &str, loc: &str) -> Meeting {
        serde_json::from_value(serde_json::json!({
            "meeting_id": id, "calendar_name": cal, "meeting_name": name,
            "meeting_date": date, "meeting_date_end": date,
            "meeting_time_start": time, "meeting_time_stop": time,
            "meeting_timezone": "UTC", "meeting_location": loc,
            "meeting_manager": ["owner"]
        }))
        .unwrap()
    }

    fn minutes(channel: &str, at: &str) -> Minutes {
        Minutes {
            channel: channel.into(),
            topic: "t".into(),
            at: NaiveDateTime::parse_from_str(at, "%Y-%m-%d %H:%M").unwrap(),
            url: format!("https://meetbot/{at}"),
        }
    }

    #[test]
    fn series_are_recurring_entries_with_their_occurrences() {
        let found = series(&[
            meeting(
                1,
                "2026-11-11",
                "16:00:00",
                "fesco",
                "FESCo Meeting",
                "meeting@fedoraproject.org",
            ),
            meeting(
                1,
                "2026-11-04",
                "16:00:00",
                "fesco",
                "FESCo Meeting",
                "meeting@fedoraproject.org",
            ),
            meeting(
                1,
                "2026-11-04",
                "16:00:00",
                "fesco",
                "FESCo Meeting",
                "meeting@fedoraproject.org",
            ),
            meeting(2, "2026-11-04", "16:00:00", "x", "One-off", ""),
        ]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].occurrences.len(), 2);
        assert_eq!(found[0].room.as_deref(), Some("meeting"));
        assert_eq!(found[0].owners, ["owner"]);
    }

    #[test]
    fn topics_come_from_the_calendar_and_the_title() {
        let s = &series(&[
            meeting(
                1,
                "2026-11-04",
                "16:00:00",
                "centos-sig-hyperscale",
                "CentOS Hyperscale SIG",
                "",
            ),
            meeting(
                1,
                "2026-11-11",
                "16:00:00",
                "centos-sig-hyperscale",
                "CentOS Hyperscale SIG",
                "",
            ),
        ])[0];
        assert_eq!(
            topic_terms(s),
            [
                "centos-sig-hyperscale",
                "centos-hyperscale-sig",
                "fedora-centos-hyperscale-sig"
            ]
        );
        let s = &series(&[
            meeting(1, "2026-11-04", "16:00:00", "fesco", "FESCo Meeting", ""),
            meeting(1, "2026-11-11", "16:00:00", "fesco", "FESCo Meeting", ""),
        ])[0];
        assert_eq!(
            topic_terms(s),
            [
                "fesco",
                "fesco-meeting",
                "fedora-fesco-meeting",
                "fedora-fesco"
            ]
        );
        assert_eq!(slug("  Fedora QA: Meeting!! "), "fedora-qa-meeting");
    }

    #[test]
    fn channels_read_as_rooms() {
        assert_eq!(
            channel_room("meeting_matrix_fedoraproject-org").as_deref(),
            Some("meeting")
        );
        assert_eq!(
            channel_room("meeting-1_matrix_fedoraproject-org").as_deref(),
            Some("meeting-1")
        );
        assert_eq!(
            channel_room("fedora-meeting-2").as_deref(),
            Some("meeting-2")
        );
        assert_eq!(channel_room("fedora-townhall"), None);
        assert_eq!(channel_room("centos-meeting"), None);
        assert!(is_video_call("https://meet.google.com/hfy-jpoo-muq"));
        assert!(is_video_call("https://us05web.zoom.us/j/1"));
        assert!(!is_video_call("meeting@fedoraproject.org"));
    }

    #[test]
    fn occurrences_are_held_when_minutes_start_near_them() {
        let s = &series(&[
            meeting(
                1,
                "2026-11-04",
                "16:00:00",
                "c",
                "M",
                "meeting@fedoraproject.org",
            ),
            meeting(
                1,
                "2026-11-11",
                "16:00:00",
                "c",
                "M",
                "meeting@fedoraproject.org",
            ),
            meeting(
                1,
                "2026-11-18",
                "16:00:00",
                "c",
                "M",
                "meeting@fedoraproject.org",
            ),
        ])[0];
        let all = [
            minutes("meeting_matrix_fedoraproject-org", "2026-11-04 16:02"),
            minutes("meeting_matrix_fedoraproject-org", "2026-11-11 16:45"),
            // Another room: held, but elsewhere; too far: not counted.
            minutes("meeting-1_matrix_fedoraproject-org", "2026-11-18 16:00"),
            minutes("meeting_matrix_fedoraproject-org", "2026-11-18 18:00"),
        ];
        let j = judge(s, &all);
        assert_eq!((j.verdict, j.held), (Verdict::Held, 3));
        assert_eq!(j.elsewhere, ["meeting-1"]);
        assert_eq!(
            j.last_minutes.as_deref(),
            Some("https://meetbot/2026-11-18 16:00")
        );
        let j = judge(s, &all[..1]);
        assert_eq!((j.verdict, j.held), (Verdict::RarelyHeld, 1));
        assert!(j.elsewhere.is_empty());
        assert_eq!(judge(s, &[]).verdict, Verdict::NotHeld);
        // No room: minutes in any channel count, and none is elsewhere.
        let mut roomless = s.clone();
        roomless.room = None;
        let mut fitting = all[2].clone();
        fitting.topic = "fedora-m".into();
        let j = judge(&roomless, &[fitting, all[3].clone()]);
        assert_eq!((j.held, j.elsewhere.len()), (1, 0));
        // A video call without minutes.
        let mut video = s.clone();
        video.room = None;
        video.location = "https://meet.google.com/x".into();
        assert_eq!(judge(&video, &[]).verdict, Verdict::VideoCall);
    }

    #[test]
    fn a_series_replaced_by_a_new_entry_is_not_reported() {
        let old = &series(&[
            meeting(
                1,
                "2025-12-17",
                "15:00:00",
                "council",
                "Council meeting",
                "",
            ),
            meeting(
                1,
                "2025-12-24",
                "15:00:00",
                "council",
                "Council meeting",
                "",
            ),
        ])[0];
        let by_title = series(&[
            meeting(
                2,
                "2026-11-11",
                "18:00:00",
                "council",
                "Fedora Council Meeting",
                "",
            ),
            meeting(
                2,
                "2026-11-18",
                "18:00:00",
                "council",
                "Fedora Council Meeting",
                "",
            ),
        ]);
        assert!(replaced(old, &by_title));
        let by_slot = series(&[
            meeting(3, "2026-11-11", "15:30:00", "council", "Something else", ""),
            meeting(3, "2026-11-18", "15:30:00", "council", "Something else", ""),
        ]);
        assert!(replaced(old, &by_slot));
        let other = series(&[
            meeting(4, "2026-11-12", "09:00:00", "council", "Hackfest", ""),
            meeting(4, "2026-11-19", "09:00:00", "council", "Hackfest", ""),
            meeting(5, "2026-11-11", "15:00:00", "other", "Council meeting", ""),
            meeting(5, "2026-11-18", "15:00:00", "other", "Council meeting", ""),
        ]);
        assert!(!replaced(old, &other));
    }

    #[test]
    fn series_that_stop_at_the_old_default_end_are_found() {
        let end = NaiveDate::from_ymd_opt(2025, 12, 31).unwrap();
        let before = series(&[
            meeting(1, "2025-12-10", "16:00:00", "a", "Stopped", ""),
            meeting(1, "2025-12-24", "16:00:00", "a", "Stopped", ""),
            meeting(2, "2025-12-10", "16:00:00", "b", "Goes on", ""),
            meeting(2, "2025-12-24", "16:00:00", "b", "Goes on", ""),
            meeting(3, "2025-10-01", "16:00:00", "c", "Long gone", ""),
            meeting(3, "2025-10-08", "16:00:00", "c", "Long gone", ""),
        ]);
        let after = [meeting(2, "2026-01-07", "16:00:00", "b", "Goes on", "")];
        let dropped = dropped_at(&before, &after, end);
        assert_eq!(dropped.len(), 1);
        assert_eq!(dropped[0].meeting_id, 1);
        // Wednesday 16:00 UTC, as the series; a Thursday, or a Wednesday
        // at 20:00, are another meeting under the same topic.
        let since = [
            minutes("meeting_matrix_fedoraproject-org", "2025-12-24 16:00"),
            minutes("meeting_matrix_fedoraproject-org", "2026-11-04 15:00"),
            minutes("meeting_matrix_fedoraproject-org", "2026-11-12 16:00"),
            minutes("meeting_matrix_fedoraproject-org", "2026-11-18 20:00"),
        ];
        assert_eq!(
            newest_since(&dropped[0], &since, end),
            Some(MinutesRef {
                date: NaiveDate::from_ymd_opt(2026, 11, 4).unwrap(),
                url: "https://meetbot/2026-11-04 15:00".into()
            })
        );
        assert_eq!(newest_since(&dropped[0], &since[..1], end), None);
    }

    fn report(verdict: Verdict, held: usize, owners_seen: &[(&str, OnDiscourse)]) -> SeriesReport {
        let s = series(&[
            meeting(
                1,
                "2026-11-04",
                "16:00:00",
                "SIGs",
                "Some SIG",
                "meeting@fedoraproject.org",
            ),
            meeting(
                1,
                "2026-11-11",
                "16:00:00",
                "SIGs",
                "Some SIG",
                "meeting@fedoraproject.org",
            ),
        ])
        .remove(0);
        SeriesReport {
            series: s,
            verdict,
            held,
            last_minutes: Some("https://meetbot/x".into()),
            elsewhere: vec![],
            searched: vec!["sigs".into(), "some-sig".into()],
            owners_seen: owners_seen
                .iter()
                .map(|(o, s)| (o.to_string(), s.clone()))
                .collect(),
        }
    }

    #[test]
    fn the_report_reads_as_text_by_verdict() {
        let mut held = report(Verdict::Held, 2, &[]);
        held.elsewhere = vec!["meeting-1".into()];
        let not = report(
            Verdict::NotHeld,
            0,
            &[
                ("a", OnDiscourse::Seen("2026-11-01".into())),
                ("b", OnDiscourse::NotFound),
                ("c", OnDiscourse::Failed("HTTP 429".into())),
            ],
        );
        let mut video = report(Verdict::VideoCall, 0, &[]);
        video.series.room = None;
        video.series.location = String::new();
        let day = |s| NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap();
        let dropped = [
            Dropped {
                series: not.series.clone(),
                searched: vec!["some-sig".into()],
                minutes_since: Some(MinutesRef {
                    date: day("2026-11-04"),
                    url: "https://meetbot/y".into(),
                }),
            },
            Dropped {
                series: not.series.clone(),
                searched: vec!["some-sig".into()],
                minutes_since: Some(MinutesRef {
                    date: day("2026-06-11"),
                    url: "https://meetbot/z".into(),
                }),
            },
            Dropped {
                series: not.series.clone(),
                searched: vec!["some-sig".into()],
                minutes_since: None,
            },
        ];
        let text = describe(
            &[not, video, held],
            &dropped,
            day("2026-08-27"),
            day("2026-11-19"),
        );
        for expected in [
            "Recurring meetings in Fedora Calendar from 2026-08-27 to 2026-11-19:\n",
            "\nNo minutes found (1):\n  Some SIG (SIGs [shared calendar]): Wed 16:00 UTC, in meeting, 2 occurrence(s)\n",
            "    searched meetbot for: sigs, some-sig\n    owners: a (seen 2026-11-01), b (not on Discourse), c (Discourse lookup failed)\n",
            "\nVideo calls, which meetbot does not log (1):\n  Some SIG (SIGs [shared calendar]): Wed 16:00 UTC, no location,",
            "\nHeld (1):\n",
            "    held 2 of 2, last https://meetbot/x\n    minutes are in meeting-1, not the room booked\n",
            "Stopped at 2025-12-31, fedocal's old default end for a series without one (3):\n",
            "    last minutes on 2026-06-11 (https://meetbot/z), none in the period\n",
            "    still held: minutes on 2026-11-04 (https://meetbot/y); its entry needs a new end\n",
            "    no minutes since (searched meetbot for: some-sig)\n",
        ] {
            assert!(text.contains(expected), "missing {expected:?} in:\n{text}");
        }
    }
    #[tokio::test]
    async fn the_report_reads_calendar_meetbot_and_discourse() {
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        let entry = |id: u64, date: &str, name: &str, loc: &str| {
            serde_json::json!({
                "meeting_id": id, "calendar_name": "cal", "meeting_name": name,
                "meeting_date": date, "meeting_date_end": date,
                "meeting_time_start": "16:00:00", "meeting_time_stop": "17:00:00",
                "meeting_timezone": "UTC", "meeting_location": loc,
                "meeting_manager": ["owner"]
            })
        };
        let answer = |entries: Vec<serde_json::Value>| {
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"arguments": {}, "meetings": entries}))
        };
        // The period: one series held, one without minutes.
        Mock::given(method("GET"))
            .and(path("/api/meetings/"))
            .and(query_param("start", "2026-10-22"))
            .respond_with(answer(vec![
                entry(1, "2026-11-04", "Held", "meeting@fedoraproject.org"),
                entry(1, "2026-11-11", "Held", "meeting@fedoraproject.org"),
                entry(2, "2026-11-05", "Quiet", "meeting-1@chat.fedoraproject.org"),
                entry(2, "2026-11-12", "Quiet", "meeting-1@chat.fedoraproject.org"),
            ]))
            .mount(&server)
            .await;
        // Around the old default end: a series that stopped there.
        Mock::given(method("GET"))
            .and(path("/api/meetings/"))
            .and(query_param("start", "2025-12-03"))
            .respond_with(answer(vec![
                entry(3, "2025-12-16", "Gone", ""),
                entry(3, "2025-12-23", "Gone", ""),
            ]))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/meetings/"))
            .and(query_param("start", "2026-01-01"))
            .respond_with(answer(vec![]))
            .mount(&server)
            .await;
        let minute = |channel: &str, at: &str| {
            serde_json::json!({
                "channel": channel, "datetime": at, "topic": "t",
                "url": {"logs": format!("https://m/{at}.log.html"), "summary": format!("https://m/{at}.html")}
            })
        };
        Mock::given(method("GET"))
            .and(path("/fragedpt/"))
            .and(query_param("srchtext", "held"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                minute("meeting_matrix_fedoraproject-org", "2026-11-04T16:01:00"),
                minute("meeting_matrix_fedoraproject-org", "2026-11-11T16:00:00"),
            ])))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/fragedpt/"))
            .and(query_param("srchtext", "gone"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!([minute(
                    "meeting_matrix_fedoraproject-org",
                    "2026-11-17T16:00:00"
                ),])),
            )
            .mount(&server)
            .await;
        // Any other topic: nothing, or an error (a warning only).
        Mock::given(method("GET"))
            .and(path("/fragedpt/"))
            .and(query_param("srchtext", "cal"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/fragedpt/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/u/owner.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "user": {"id": 1, "username": "owner", "last_seen_at": "2026-11-18T10:00:00Z"}
            })))
            .mount(&server)
            .await;
        let client = Client::with_base_url(&server.uri());
        let sources = Sources {
            meetbot: server.uri(),
            discourse: server.uri(),
            cache: false,
        };
        let now = NaiveDateTime::parse_from_str("2026-11-19 12:00", "%Y-%m-%d %H:%M")
            .unwrap()
            .and_utc();
        let (reports, dropped) = super::report(&client, &sources, now, 4, false)
            .await
            .unwrap();
        let shown: Vec<(String, Verdict, usize)> = reports
            .iter()
            .map(|r| (r.series.name.clone(), r.verdict, r.held))
            .collect();
        assert_eq!(
            shown,
            [
                ("Quiet".to_string(), Verdict::NotHeld, 0),
                ("Held".to_string(), Verdict::Held, 2)
            ]
        );
        assert_eq!(
            reports[0].owners_seen.get("owner"),
            Some(&OnDiscourse::Seen("2026-11-18".to_string()))
        );
        assert!(reports[1].owners_seen.is_empty());
        assert_eq!(dropped.len(), 1);
        assert_eq!(dropped[0].series.name, "Gone");
        // meetbot's client puts its own address on the links.
        assert!(
            dropped[0]
                .minutes_since
                .as_ref()
                .unwrap()
                .url
                .ends_with("/2026-11-17T16:00:00.html")
        );
        assert!(Sources::default().cache);
    }
    #[test]
    fn without_a_room_the_topic_must_be_the_series() {
        let s = &series(&[
            meeting(
                1,
                "2026-11-04",
                "15:30:00",
                "CoreOS",
                "Fedora CoreOS Group Weekly Meeting",
                "",
            ),
            meeting(
                1,
                "2026-11-11",
                "15:30:00",
                "CoreOS",
                "Fedora CoreOS Group Weekly Meeting",
                "",
            ),
        ])[0];
        assert!(topic_fits(s, "fedora-coreos-meeting"));
        let sig = &series(&[
            meeting(2, "2026-11-04", "16:00:00", "SIGs", "Quiet SIG", ""),
            meeting(2, "2026-11-11", "16:00:00", "SIGs", "Quiet SIG", ""),
        ])[0];
        // `sigs` is in many topics, but not as a whole word in this one.
        assert!(!topic_fits(sig, "fedora-ai-ml-sig"));
        let other = Minutes {
            channel: "meeting-2_matrix_fedoraproject-org".into(),
            topic: "fedora-ai-ml-sig".into(),
            at: NaiveDateTime::parse_from_str("2026-11-04 16:00", "%Y-%m-%d %H:%M").unwrap(),
            url: "u".into(),
        };
        assert_eq!(judge(sig, &[other]).verdict, Verdict::NotHeld);
    }

    #[test]
    fn occurrences_not_yet_due_are_left_out() {
        let found = series(&[
            meeting(1, "2026-11-12", "16:00:00", "c", "M", ""),
            meeting(1, "2026-11-19", "16:00:00", "c", "M", ""),
            meeting(2, "2026-11-19", "16:00:00", "c", "Later", ""),
            meeting(2, "2026-11-26", "16:00:00", "c", "Later", ""),
        ]);
        let cutoff = NaiveDateTime::parse_from_str("2026-11-19 10:00", "%Y-%m-%d %H:%M")
            .unwrap()
            .and_utc();
        let kept = up_to(found, cutoff);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].occurrences.len(), 1);
    }

    #[test]
    fn times_in_the_week_compare_across_midnight() {
        let t = |s: &str| {
            NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M")
                .unwrap()
                .and_utc()
        };
        // Wednesday 23:30 and the next Thursday 00:30, weeks apart.
        assert_eq!(
            week_distance(t("2025-12-17 23:30"), t("2026-04-02 00:30")),
            60
        );
        // Sunday 23:30 and Monday 00:15 wrap around the week.
        assert_eq!(
            week_distance(t("2026-11-15 23:30"), t("2026-11-16 00:15")),
            45
        );
        assert_eq!(
            week_distance(t("2026-11-16 12:00"), t("2026-11-19 12:00")),
            3 * 24 * 60
        );
    }

    #[test]
    fn one_shared_word_is_not_a_replacement() {
        let old = &series(&[
            meeting(1, "2025-12-15", "09:00:00", "SIGs", "SIG meeting", ""),
            meeting(1, "2025-12-22", "09:00:00", "SIGs", "SIG meeting", ""),
        ])[0];
        let current = series(&[
            meeting(2, "2026-11-11", "16:00:00", "SIGs", "Hyperscale SIG", ""),
            meeting(2, "2026-11-18", "16:00:00", "SIGs", "Hyperscale SIG", ""),
        ]);
        assert!(!replaced(old, &current));
    }

    #[test]
    fn an_evening_in_the_west_on_the_last_day_still_ends_there() {
        let end = NaiveDate::from_ymd_opt(2025, 12, 31).unwrap();
        // 2025-12-31 18:00 in New York is 23:00 UTC; the next week's
        // would be 2026-01-07, not listed.
        let before = series(&[
            meeting(1, "2025-12-25", "23:00:00", "a", "West", ""),
            meeting(1, "2026-01-01", "23:00:00", "a", "West", ""),
        ]);
        let after = [meeting(1, "2026-01-01", "23:00:00", "a", "West", "")];
        assert_eq!(dropped_at(&before, &after, end).len(), 1);
    }
}
