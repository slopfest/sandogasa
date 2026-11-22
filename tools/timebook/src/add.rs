// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `timebook add`: book a meeting held on the Nth weekday of each month,
//! like one of your own entries.
//!
//! Fedora Calendar repeats an entry every 7, 14, 21 or 28 days only, so a
//! monthly meeting booked as "every 28 days" slips a week whenever a month
//! has five of its weekday. `add` books the right days instead: the dates
//! are grouped into runs 28 days apart, each run one fedocal series (a
//! date after a five-week gap starts a new one), copying everything else
//! from the entry given. Dates where the calendar already has an entry
//! of that title are skipped, so running it again extends the booking.
//!
//! `--replace` turns the entry's own series into the first run: one save
//! ("Save all") stops it repeating from its next occurrence on, which
//! drops the later occurrences (fedocal keeps the earlier ones as a
//! separate entry), and a second moves that occurrence to the first day
//! and sets the run's repeat. Moving it in the first save would leave a
//! copy of the old series behind. The entry keeps its id and page.

use std::collections::BTreeSet;
use std::str::FromStr;

use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveTime, TimeZone, Utc, Weekday};
use chrono_tz::Tz;
use sandogasa_fedocal::room::Room;
use sandogasa_fedocal::session::{Changes, EntryForm, Repeat, Session};
use sandogasa_fedocal::{Client, Meeting, MeetingQuery};
use serde::Serialize;

use crate::book::{self, Booking, Slot};

/// Days between the occurrences of one run: fedocal's longest repeat.
pub const RUN_DAYS: i64 = 28;

/// The most entries' dates one run of `add` books: ten years of a
/// monthly meeting.
pub const MAX_DATES: usize = 120;

/// Which week of the month a meeting falls in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Week {
    /// The first to fourth: every month has one.
    Nth(u32),
    Last,
}

/// A day of each month: its week and weekday, as in `3rd-wed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Monthly {
    pub week: Week,
    pub weekday: Weekday,
}

impl Monthly {
    /// Its day in `year`'s `month`.
    pub fn day_in(&self, year: i32, month: u32) -> Option<NaiveDate> {
        match self.week {
            Week::Nth(n) => {
                NaiveDate::from_weekday_of_month_opt(year, month, self.weekday, n as u8)
            }
            Week::Last => {
                let (y, m) = if month == 12 {
                    (year + 1, 1)
                } else {
                    (year, month + 1)
                };
                let last = NaiveDate::from_ymd_opt(y, m, 1)?.pred_opt()?;
                let back = (last.weekday().num_days_from_monday() + 7
                    - self.weekday.num_days_from_monday())
                    % 7;
                Some(last - Duration::days(back.into()))
            }
        }
    }
}

/// Where a booking ends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum End {
    /// The last day a meeting may fall on.
    Until(NaiveDate),
    /// How many days from the first, booked already or not.
    Count(usize),
    /// A span from the first day, as `6m` or `1y` (see
    /// [`sandogasa_cli::date::span_end`]).
    For(String),
}

/// One fedocal entry of a booking: a single meeting, or a series every
/// [`RUN_DAYS`] days.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Run {
    pub first: NaiveDate,
    pub last: NaiveDate,
    pub occurrences: usize,
}

impl Run {
    /// The repeat fedocal is given for it; `None` for a single meeting.
    pub fn repeat(&self) -> Option<Repeat> {
        (self.occurrences > 1).then_some(Repeat {
            every_days: RUN_DAYS as u32,
            until: self.last,
        })
    }
}

/// What `add` was asked to book.
pub struct AddArgs {
    /// The entry to copy: its id or page, or its meeting's calendar.
    pub like: String,
    /// In a calendar with several meetings, part of the title of the one
    /// meant.
    pub meeting: Option<String>,
    pub monthly: Monthly,
    /// The last day (`--until`), or the span from today (`--for`), already
    /// turned into one.
    pub end: End,
    /// Turn the entry's series into the first run.
    pub replace: bool,
    pub dry_run: bool,
    pub yes: bool,
    /// Print the outcome as JSON, the messages going to stderr.
    pub json: bool,
}

/// One entry `add` writes, or would.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Planned {
    #[serde(flatten)]
    pub run: Run,
    /// `moved` (the entry `--replace` changes) or `added`.
    pub action: &'static str,
    /// Days that need a look: a clash in the meeting's room, or a time
    /// a daylight-saving change skips.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// What `add` did, for `--json`.
#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct AddOutcome {
    /// `booked`, `declined`, `dry_run` or `unchanged`.
    pub outcome: &'static str,
    pub calendar: String,
    pub title: String,
    pub entries: Vec<Planned>,
    /// Days already booked, left alone.
    pub skipped: Vec<NaiveDate>,
}

/// The entry an edit form shows, as `add` needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Template {
    pub calendar: String,
    pub title: String,
    pub location: String,
    pub tz: Tz,
    pub start: NaiveTime,
    pub stop: NaiveTime,
    /// The occurrence the form shows: a series' next one.
    pub date: NaiveDate,
    pub recurring: bool,
    pub frequency: String,
    pub end_repeats: String,
}

impl Template {
    /// Read `form`; an error naming what does not read.
    pub fn of(form: &EntryForm) -> Result<Template, String> {
        let get = |name: &str| form.get(name).unwrap_or("").trim().to_string();
        let tz: Tz = get("meeting_timezone").parse().map_err(|_| {
            format!(
                "the entry's time zone `{}` does not read",
                get("meeting_timezone")
            )
        })?;
        let time = |name: &str| {
            NaiveTime::parse_from_str(&get(name), "%H:%M")
                .map_err(|_| format!("the entry's {name} `{}` does not read", get(name)))
        };
        let date = NaiveDate::parse_from_str(&get("meeting_date"), "%Y-%m-%d")
            .map_err(|_| format!("the entry's date `{}` does not read", get("meeting_date")))?;
        let calendar = get("calendar_name");
        if calendar.is_empty() {
            return Err("the entry's form names no calendar".into());
        }
        if get("meeting_name").is_empty() {
            return Err("the entry has no title to find its other days by".into());
        }
        Ok(Template {
            calendar,
            title: get("meeting_name"),
            location: get("meeting_location"),
            tz,
            start: time("meeting_time_start")?,
            stop: time("meeting_time_stop")?,
            date,
            recurring: form.recurring(),
            frequency: get("frequency"),
            end_repeats: get("end_repeats"),
        })
    }

    /// The meeting on `day`, in UTC; `None` in a daylight-saving gap.
    pub fn slot(&self, day: NaiveDate) -> Option<Slot> {
        let at = |t: NaiveTime| {
            self.tz
                .from_local_datetime(&day.and_time(t))
                .earliest()
                .map(|t| t.with_timezone(&Utc))
        };
        let start = at(self.start)?;
        let mut end = at(self.stop)?;
        if end <= start {
            end += Duration::days(1);
        }
        Some(Slot { start, end })
    }

    /// `day` at the meeting's time, local and in UTC.
    pub fn when(&self, day: NaiveDate) -> String {
        match self.slot(day) {
            Some(s) => format!(
                "{} ({})",
                s.start
                    .with_timezone(&self.tz)
                    .format("%a %Y-%m-%d %H:%M %Z"),
                s.start.format("%H:%M UTC")
            ),
            None => format!("{} (not a time in {})", day.format("%a %Y-%m-%d"), self.tz),
        }
    }
}

/// The meeting's days from `from` on (that day included) to `end`; an
/// error when there are none, or more than [`MAX_DATES`].
pub fn dates(monthly: Monthly, from: NaiveDate, end: &End) -> Result<Vec<NaiveDate>, String> {
    let end = match end {
        End::For(span) => End::Until(sandogasa_cli::date::span_end(from, span)?),
        other => other.clone(),
    };
    let (mut year, mut month) = (from.year(), from.month());
    let mut out = Vec::new();
    loop {
        if let Some(day) = monthly.day_in(year, month).filter(|d| *d >= from) {
            match end {
                End::Until(until) if day > until => break,
                End::Count(n) if out.len() == n => break,
                _ => {}
            }
            if out.len() == MAX_DATES {
                return Err(format!(
                    "that is more than {MAX_DATES} meetings; book fewer at a time"
                ));
            }
            out.push(day);
        }
        (year, month) = if month == 12 {
            (year + 1, 1)
        } else {
            (year, month + 1)
        };
    }
    if out.is_empty() {
        return Err(match end {
            End::Until(until) => format!("no such day from {from} to {until}"),
            End::Count(_) => "--count must be at least 1".to_string(),
            End::For(_) => unreachable!("a span is read as a last day"),
        });
    }
    Ok(out)
}

/// `dates` (in order) as runs: each date [`RUN_DAYS`] after the one before
/// joins its run, and any other gap starts a new one.
pub fn runs(dates: &[NaiveDate]) -> Vec<Run> {
    let mut out: Vec<Run> = Vec::new();
    for &d in dates {
        match out.last_mut() {
            Some(run) if d - run.last == Duration::days(RUN_DAYS) => {
                run.last = d;
                run.occurrences += 1;
            }
            _ => out.push(Run {
                first: d,
                last: d,
                occurrences: 1,
            }),
        }
    }
    out
}

/// The days, in their own time zone, on which `calendar` holds an entry
/// titled `title` (in any case), leaving out entry `except`.
pub fn booked_days(
    meetings: &[Meeting],
    calendar: &str,
    title: &str,
    except: Option<u64>,
) -> BTreeSet<NaiveDate> {
    let title = title.trim().to_lowercase();
    meetings
        .iter()
        .filter(|m| m.calendar_name == calendar)
        .filter(|m| m.meeting_name.trim().to_lowercase() == title)
        .filter(|m| Some(m.meeting_id) != except)
        .filter_map(Meeting::local_date)
        .collect()
}

/// The entries to write for `dates`, leaving out the days in `booked`;
/// with `replace`, the first is entry `id` moved. Clashes are looked up
/// in `bookings` when the meeting is in a shared room, leaving out its
/// own entries.
pub fn plan(
    template: &Template,
    dates: &[NaiveDate],
    booked: &BTreeSet<NaiveDate>,
    replace: bool,
    id: u64,
    bookings: &[Booking],
) -> (Vec<Planned>, Vec<NaiveDate>) {
    let (skipped, free): (Vec<NaiveDate>, Vec<NaiveDate>) =
        dates.iter().partition(|d| booked.contains(d));
    let room = Room::from_location(&template.location).map(|r| r.name);
    let title = template.title.trim().to_lowercase();
    let others: Vec<Booking> = bookings
        .iter()
        .filter(|b| {
            b.meeting_id != id
                && !(b.calendar == template.calendar && b.name.trim().to_lowercase() == title)
        })
        .cloned()
        .collect();
    let planned = runs(&free)
        .into_iter()
        .enumerate()
        .map(|(i, run)| {
            let mut warnings = Vec::new();
            for d in run_days(&run) {
                let Some(slot) = template.slot(d) else {
                    warnings.push(format!(
                        "{} {} does not exist in {}: a daylight-saving change skips it",
                        d.format("%a %Y-%m-%d"),
                        template.start.format("%H:%M"),
                        template.tz
                    ));
                    continue;
                };
                let Some(room) = &room else { continue };
                for s in book::check(room, &[slot], &others).slots {
                    for b in s.clashes {
                        warnings.push(format!(
                            "{room} taken on {}: {} ({}) {}-{} UTC",
                            d.format("%a %Y-%m-%d"),
                            b.name,
                            b.calendar,
                            b.start.format("%H:%M"),
                            b.end.format("%H:%M")
                        ));
                    }
                }
            }
            Planned {
                run,
                action: if replace && i == 0 { "moved" } else { "added" },
                warnings,
            }
        })
        .collect();
    (planned, skipped)
}

/// The preview: the meeting, what `--replace` takes away, and each entry.
pub fn describe(
    template: &Template,
    planned: &[Planned],
    skipped: &[NaiveDate],
    replaced: Option<(u64, &[NaiveDate])>,
) -> String {
    let location = if template.location.is_empty() {
        "no location".to_string()
    } else {
        format!("at {}", template.location)
    };
    let mut out = format!(
        "\"{}\" ({}), {}-{} {}, {location}:\n",
        template.title,
        template.calendar,
        template.start.format("%H:%M"),
        template.stop.format("%H:%M"),
        template.tz
    );
    if let Some((id, coming)) = replaced {
        const SHOWN: usize = 3;
        let mut days: Vec<String> = coming
            .iter()
            .take(SHOWN)
            .map(|d| d.format("%a %Y-%m-%d").to_string())
            .collect();
        if coming.len() > SHOWN {
            days.push(format!("and {} more", coming.len() - SHOWN));
        }
        let every = if template.frequency.is_empty() {
            String::new()
        } else {
            format!(
                " (every {} days until {})",
                template.frequency, template.end_repeats
            )
        };
        out.push_str(&format!(
            "  replaces entry {id}'s occurrences from {} on{every}{}\n",
            template.date.format("%a %Y-%m-%d"),
            if days.is_empty() {
                String::new()
            } else {
                format!(", coming: {}", days.join(", "))
            }
        ));
    }
    for p in planned {
        let label = match (p.action, replaced) {
            ("moved", Some((id, _))) => format!("entry {id} becomes"),
            _ if p.run.occurrences > 1 => "new series".to_string(),
            _ => "new entry".to_string(),
        };
        let times = if p.run.occurrences > 1 {
            format!(
                ", then every {RUN_DAYS} days to {}, {} times",
                p.run.last.format("%a %Y-%m-%d"),
                p.run.occurrences
            )
        } else {
            String::new()
        };
        out.push_str(&format!(
            "  {label:<18} {}{times}\n",
            template.when(p.run.first)
        ));
        for c in &p.warnings {
            out.push_str(&format!("    {c}\n"));
        }
    }
    if !skipped.is_empty() {
        let days: Vec<String> = skipped
            .iter()
            .map(|d| d.format("%a %Y-%m-%d").to_string())
            .collect();
        out.push_str(&format!(
            "  already booked, left alone: {}\n",
            days.join(", ")
        ));
    }
    out
}

/// Book the meeting `args` asks for from `now` on: log in, read the
/// entry, show what would be written, confirm, write it, and read it
/// back. `confirm` asks at the terminal; `login` logs in (blocking).
pub async fn run(
    client: &Client,
    args: &AddArgs,
    now: DateTime<Utc>,
    confirm: impl Fn(&str, bool) -> Result<bool, String>,
    login: impl FnOnce() -> Result<Session, sandogasa_fedocal::Error> + Send + 'static,
) -> Result<AddOutcome, String> {
    let say = |line: String| {
        if args.json {
            eprintln!("{line}");
        } else {
            println!("{line}");
        }
    };
    let id = crate::edit::entry_named(client, &args.like, args.meeting.as_deref()).await?;
    eprintln!("Logging in to Fedora Calendar with your Kerberos ticket...");
    let session = tokio::task::spawn_blocking(login)
        .await
        .map_err(|e| format!("the login stopped: {e}"))?
        .map_err(|e| e.to_string())?;
    let form = session.entry(id).await.map_err(|e| e.to_string())?;
    let template = Template::of(&form)?;
    let url = format!("{}/meeting/{id}/", client.base_url());
    // From today, or tomorrow once today's meeting has begun.
    let mut today = now.with_timezone(&template.tz).date_naive();
    if template.slot(today).is_some_and(|s| s.start <= now) {
        today = today.succ_opt().unwrap_or(today);
    }
    let days = dates(args.monthly, today, &args.end)?;
    let (first, last) = (days[0], days[days.len() - 1]);

    // One request for the calendar's entries over the booking (every
    // calendar's when the meeting is in a shared room), from the entry's
    // next occurrence on, to see which of its own days are coming.
    let from = first.min(template.date.max(today));
    let between = (from - Duration::days(1), last + Duration::days(1));
    eprintln!(
        "Reading Fedora Calendar from {} to {}...",
        between.0, between.1
    );
    let in_room = Room::from_location(&template.location).is_some();
    let mut query = MeetingQuery::default().between(between.0, between.1);
    if !in_room {
        query = query.calendar(template.calendar.clone());
    }
    let meetings = client.meetings(&query).await.map_err(|e| e.to_string())?;
    let except = args.replace.then_some(id);
    let booked = booked_days(&meetings, &template.calendar, &template.title, except);
    let bookings = if in_room {
        book::bookings(&meetings)
    } else {
        Vec::new()
    };
    let (planned, skipped) = plan(&template, &days, &booked, args.replace, id, &bookings);
    let coming: Vec<NaiveDate> = {
        let mut c: Vec<NaiveDate> = meetings
            .iter()
            .filter(|m| m.meeting_id == id)
            .filter_map(Meeting::local_date)
            .filter(|d| *d >= template.date)
            .collect();
        c.sort();
        c.dedup();
        c
    };
    // A series is copied only when it is one of this booking's own
    // runs: ending within it, every coming occurrence on one of its days.
    // Otherwise its other days would stay beside the new entries.
    if template.recurring && !args.replace {
        let ends = NaiveDate::parse_from_str(&template.end_repeats, "%Y-%m-%d").ok();
        let ours = ends.is_some_and(|e| e <= last) && coming.iter().all(|d| days.contains(d));
        if !ours {
            return Err(format!(
                "{url} repeats every {} days, on days besides the ones asked for, which \
                 would stay beside the new entries: add --replace to turn it into them, or \
                 end it first with `timebook edit {id} --until <day>`",
                template.frequency
            ));
        }
    }
    let outcome = |outcome, entries: Vec<Planned>| AddOutcome {
        outcome,
        calendar: template.calendar.clone(),
        title: template.title.clone(),
        entries,
        skipped: skipped.clone(),
    };
    if planned.is_empty() {
        if args.replace {
            return Err(format!(
                "every day asked for is booked already, so nothing would take {url}'s place; \
                 end it with `timebook edit {id} --until <day>` instead"
            ));
        }
        say(describe(&template, &planned, &skipped, None));
        say("Nothing to book: every day asked for is booked already.".to_string());
        return Ok(outcome("unchanged", planned));
    }
    let replaced = args.replace.then_some((id, coming.as_slice()));
    say(describe(&template, &planned, &skipped, replaced));
    if args.dry_run {
        session
            .can_add(&template.calendar)
            .await
            .map_err(|e| format!("you may not add to {}: {e}", template.calendar))?;
        say("Dry run: you may add to the calendar; nothing changed.".to_string());
        return Ok(outcome("dry_run", planned));
    }
    let question = format!("Make these {} change(s)?", planned.len());
    if !args.yes && !confirm(&question, false)? {
        say("Nothing changed.".to_string());
        return Ok(outcome("declined", planned));
    }

    // What has been written, for the error when a write fails: once the
    // series is replaced the same command would refuse (the entry no
    // longer repeats), so the way on is without --replace.
    let mut written: Vec<NaiveDate> = Vec::new();
    let mut replaced_done = false;
    let stopped = |e: String, written: &[NaiveDate], replaced_done: bool| {
        let done: Vec<String> = written.iter().map(|d| d.to_string()).collect();
        let booked = if done.is_empty() {
            String::new()
        } else {
            format!(" (booked already: {})", done.join(", "))
        };
        let again = if replaced_done {
            "running the command again without --replace books the rest"
        } else {
            "running the same command again books the rest"
        };
        format!("{e}{booked}; {again}")
    };
    for p in &planned {
        let repeat = p.run.repeat();
        if p.action == "moved" {
            // First stop the series repeating from its next occurrence on,
            // then move that occurrence and give it the run's repeat.
            if template.recurring {
                session
                    .edit(id, &Changes::default().single(true).whole_series(true))
                    .await
                    .map_err(|e| {
                        stopped(
                            format!("cannot end the series of {url}: {e}"),
                            &written,
                            false,
                        )
                    })?;
            }
            let mut changes = Changes::default().move_to(p.run.first);
            if let Some(r) = repeat {
                changes = changes.repeat(r);
            }
            session.edit(id, &changes).await.map_err(|e| {
                format!(
                    "{url} does not repeat now, but stays on {} instead of moving to {}: \
                     {e}; running the same command again moves it",
                    template.date, p.run.first
                )
            })?;
            replaced_done = true;
        } else {
            session
                .add_like(&form, p.run.first, repeat)
                .await
                .map_err(|e| {
                    stopped(
                        format!("cannot add {}: {e}", p.run.first),
                        &written,
                        replaced_done,
                    )
                })?;
        }
        written.push(p.run.first);
    }

    // Read back, so "booked" means the calendar shows every day.
    let after = client
        .meetings(
            &MeetingQuery::default()
                .calendar(template.calendar.clone())
                .between(between.0, between.1),
        )
        .await
        .map_err(|e| format!("booked, but cannot read the calendar back: {e}"))?;
    let shown = booked_days(&after, &template.calendar, &template.title, None);
    let missing: Vec<String> = planned
        .iter()
        .flat_map(|p| run_days(&p.run))
        .filter(|d| !shown.contains(d))
        .map(|d| d.to_string())
        .collect();
    if !missing.is_empty() {
        return Err(format!(
            "Fedora Calendar accepted the entries but does not show {} yet; check {}/{}/",
            missing.join(", "),
            client.base_url(),
            template.calendar
        ));
    }
    let entries: usize = planned.iter().map(|p| p.run.occurrences).sum();
    say(format!(
        "Booked {entries} meeting(s) in {} entr{}: {}/{}/",
        planned.len(),
        if planned.len() == 1 { "y" } else { "ies" },
        client.base_url(),
        template.calendar
    ));
    Ok(outcome("booked", planned))
}

impl FromStr for Monthly {
    type Err = String;

    /// `<week>-<weekday>`: the week `1st` to `4th` (or `first` to
    /// `fourth`) or `last`, the weekday `mon` to `sun` or in full.
    fn from_str(s: &str) -> Result<Self, String> {
        let bad = || format!("`{s}` is not a day of the month: give e.g. 3rd-wed or last-fri");
        let (week, day) = s.trim().split_once('-').ok_or_else(bad)?;
        let week = match week.to_ascii_lowercase().as_str() {
            "1st" | "first" => Week::Nth(1),
            "2nd" | "second" => Week::Nth(2),
            "3rd" | "third" => Week::Nth(3),
            "4th" | "fourth" => Week::Nth(4),
            "last" => Week::Last,
            _ => return Err(bad()),
        };
        let weekday = day.parse::<Weekday>().map_err(|_| bad())?;
        Ok(Monthly { week, weekday })
    }
}

/// The days of `run`.
fn run_days(run: &Run) -> impl Iterator<Item = NaiveDate> + '_ {
    (0..run.occurrences).map(|i| run.first + Duration::days(RUN_DAYS * i as i64))
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn day(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    fn third_wed() -> Monthly {
        "3rd-wed".parse().unwrap()
    }

    #[test]
    fn a_day_of_the_month_reads_as_its_week_and_weekday() {
        assert_eq!(
            third_wed(),
            Monthly {
                week: Week::Nth(3),
                weekday: Weekday::Wed
            }
        );
        let m: Monthly = "First-Monday".parse().unwrap();
        assert_eq!(m.week, Week::Nth(1));
        assert_eq!(m.weekday, Weekday::Mon);
        assert_eq!("last-fri".parse::<Monthly>().unwrap().week, Week::Last);
        assert_eq!("4th-sun".parse::<Monthly>().unwrap().week, Week::Nth(4));
        assert_eq!("second-tue".parse::<Monthly>().unwrap().week, Week::Nth(2));
        assert_eq!("third-thu".parse::<Monthly>().unwrap().week, Week::Nth(3));
        assert_eq!("fourth-sat".parse::<Monthly>().unwrap().week, Week::Nth(4));
        assert_eq!("2nd-wed".parse::<Monthly>().unwrap().week, Week::Nth(2));
        for bad in ["3rd", "5th-wed", "3rd-wen", "", "-wed"] {
            assert!(bad.parse::<Monthly>().is_err(), "{bad}");
        }
    }

    #[test]
    fn each_month_has_its_day() {
        assert_eq!(third_wed().day_in(2026, 12), Some(day("2026-12-16")));
        assert_eq!(third_wed().day_in(2027, 1), Some(day("2027-01-20")));
        let last_fri: Monthly = "last-fri".parse().unwrap();
        assert_eq!(last_fri.day_in(2026, 12), Some(day("2026-12-25")));
        assert_eq!(last_fri.day_in(2027, 2), Some(day("2027-02-26")));
        // The last day of the month is itself the weekday.
        let last_thu: Monthly = "last-thu".parse().unwrap();
        assert_eq!(last_thu.day_in(2026, 12), Some(day("2026-12-31")));
    }

    #[test]
    fn the_days_run_from_today_to_the_end() {
        let from = day("2026-11-22");
        let by_count = dates(third_wed(), from, &End::Count(3)).unwrap();
        assert_eq!(
            by_count,
            [day("2026-12-16"), day("2027-01-20"), day("2027-02-17")]
        );
        // This month's day is still to come: it counts.
        let early = dates(third_wed(), day("2026-11-02"), &End::Count(1)).unwrap();
        assert_eq!(early, [day("2026-11-18")]);
        let by_day = dates(third_wed(), from, &End::Until(day("2027-01-20"))).unwrap();
        assert_eq!(by_day, [day("2026-12-16"), day("2027-01-20")]);
        assert!(dates(third_wed(), from, &End::Until(day("2026-12-01"))).is_err());
        assert!(dates(third_wed(), from, &End::Count(0)).is_err());
        assert!(dates(third_wed(), from, &End::Count(MAX_DATES)).is_ok());
        let err = dates(third_wed(), from, &End::Count(MAX_DATES + 1)).unwrap_err();
        assert!(err.contains("more than"), "{err}");
        let by_span = dates(third_wed(), from, &End::For("3m".into())).unwrap();
        assert_eq!(
            by_span,
            [day("2026-12-16"), day("2027-01-20"), day("2027-02-17")]
        );
        assert!(dates(third_wed(), from, &End::For("3x".into())).is_err());
    }

    #[test]
    fn days_four_weeks_apart_share_a_series() {
        let days = dates(third_wed(), day("2026-11-22"), &End::Count(13)).unwrap();
        let got: Vec<(NaiveDate, usize)> = runs(&days)
            .iter()
            .map(|r| (r.first, r.occurrences))
            .collect();
        assert_eq!(
            got,
            [
                (day("2026-12-16"), 1),
                (day("2027-01-20"), 3),
                (day("2027-04-21"), 3),
                (day("2027-07-21"), 3),
                (day("2027-10-20"), 3),
            ]
        );
        let series = &runs(&days)[1];
        assert_eq!(series.last, day("2027-03-17"));
        assert_eq!(
            series.repeat(),
            Some(Repeat {
                every_days: 28,
                until: day("2027-03-17")
            })
        );
        assert_eq!(runs(&days)[0].repeat(), None);
        assert!(runs(&[]).is_empty());
    }

    fn meeting(id: u64, calendar: &str, name: &str, utc: &str) -> Meeting {
        let (date, time) = utc.split_once(' ').unwrap();
        serde_json::from_value(serde_json::json!({
            "meeting_id": id, "calendar_name": calendar, "meeting_name": name,
            "meeting_date": date, "meeting_date_end": date,
            "meeting_time_start": format!("{time}:00"), "meeting_time_stop": "23:59:00",
            "meeting_timezone": "US/Eastern",
            "meeting_location": "meeting-1@chat.fedoraproject.org"
        }))
        .unwrap()
    }

    #[test]
    fn booked_days_are_the_titles_own_in_its_time_zone() {
        let meetings = [
            meeting(7, "hs", "Hangout", "2026-12-02 22:00"),
            // 01:00 UTC is still the day before in New York.
            meeting(9, "hs", " hangout ", "2027-01-21 01:00"),
            meeting(10, "hs", "Other", "2027-02-17 22:00"),
            meeting(11, "qa", "Hangout", "2027-03-17 21:00"),
        ];
        let all = booked_days(&meetings, "hs", "Hangout", None);
        assert_eq!(
            all.into_iter().collect::<Vec<_>>(),
            [day("2026-12-02"), day("2027-01-20")]
        );
        let but_7 = booked_days(&meetings, "hs", "Hangout", Some(7));
        assert_eq!(but_7.into_iter().collect::<Vec<_>>(), [day("2027-01-20")]);
    }

    fn form(location: &str, recurring: bool) -> EntryForm {
        let mut f = EntryForm::default();
        f.fields = [
            ("calendar_name", "hs"),
            ("meeting_name", "Hangout"),
            ("meeting_date", "2026-12-02"),
            ("meeting_date_end", "2026-12-02"),
            ("meeting_time_start", "17:00"),
            ("meeting_time_stop", "18:00"),
            ("meeting_timezone", "US/Eastern"),
            ("meeting_location", location),
            ("frequency", if recurring { "28" } else { "" }),
            ("end_repeats", if recurring { "2037-12-31" } else { "" }),
        ]
        .iter()
        .map(|(n, v)| (n.to_string(), v.to_string()))
        .collect();
        f.submits = if recurring {
            vec!["Save".into(), "Save all".into()]
        } else {
            vec!["Save".into()]
        };
        f
    }

    #[test]
    fn the_template_reads_the_time_of_the_entry_in_its_zone() {
        let t = Template::of(&form("", true)).unwrap();
        assert_eq!(t.calendar, "hs");
        assert!(t.recurring);
        assert_eq!(t.frequency, "28");
        // Standard time, then daylight saving time: the local time stays.
        assert_eq!(
            t.when(day("2026-12-16")),
            "Wed 2026-12-16 17:00 EST (22:00 UTC)"
        );
        assert_eq!(
            t.when(day("2027-03-17")),
            "Wed 2027-03-17 17:00 EDT (21:00 UTC)"
        );
        let slot = t.slot(day("2026-12-16")).unwrap();
        assert_eq!(slot.end - slot.start, Duration::hours(1));
        // A meeting past midnight ends the next day.
        let mut late = t.clone();
        late.stop = NaiveTime::from_hms_opt(1, 0, 0).unwrap();
        let slot = late.slot(day("2026-12-16")).unwrap();
        assert_eq!(slot.end - slot.start, Duration::hours(8));
        // A time in a daylight-saving gap is no time.
        let mut gap = t.clone();
        gap.start = NaiveTime::from_hms_opt(2, 30, 0).unwrap();
        assert!(gap.slot(day("2027-03-14")).is_none());
        assert!(gap.when(day("2027-03-14")).contains("not a time"));

        let mut bad = form("", true);
        bad.fields[6].1 = "Mars/Olympus".into();
        assert!(Template::of(&bad).unwrap_err().contains("time zone"));
        let mut bad = form("", true);
        bad.fields[4].1 = "5pm".into();
        assert!(
            Template::of(&bad)
                .unwrap_err()
                .contains("meeting_time_start")
        );
        let mut bad = form("", true);
        bad.fields[2].1 = "soon".into();
        assert!(Template::of(&bad).unwrap_err().contains("date"));
        let mut bad = form("", true);
        bad.fields[0].1 = "".into();
        assert!(Template::of(&bad).unwrap_err().contains("calendar"));
        let mut bad = form("", true);
        bad.fields[1].1 = " ".into();
        assert!(Template::of(&bad).unwrap_err().contains("no title"));
    }

    #[test]
    fn the_plan_skips_booked_days_and_names_room_clashes() {
        let t = Template::of(&form("meeting-1@chat.fedoraproject.org", true)).unwrap();
        let days = dates(third_wed(), day("2026-11-22"), &End::Count(5)).unwrap();
        let booked: BTreeSet<NaiveDate> = [day("2027-04-21")].into();
        let bookings = book::bookings(&[
            // The entry being replaced and its own title do not clash.
            meeting(7, "hs", "Hangout", "2026-12-16 22:00"),
            meeting(12, "hs", "Hangout", "2027-01-20 22:00"),
            meeting(13, "qa", "QA", "2027-02-17 22:30"),
        ]);
        let (planned, skipped) = plan(&t, &days, &booked, true, 7, &bookings);
        assert_eq!(skipped, [day("2027-04-21")]);
        assert_eq!(planned.len(), 2);
        assert_eq!(planned[0].action, "moved");
        assert_eq!(planned[0].run.first, day("2026-12-16"));
        assert!(planned[0].warnings.is_empty());
        assert_eq!(planned[1].action, "added");
        assert_eq!(planned[1].run.occurrences, 3);
        assert_eq!(
            planned[1].warnings,
            ["meeting-1 taken on Wed 2027-02-17: QA (qa) 22:30-23:59 UTC"]
        );
        let coming = [
            day("2026-12-02"),
            day("2026-12-30"),
            day("2027-01-27"),
            day("2027-02-24"),
        ];
        let text = describe(&t, &planned, &skipped, Some((7, &coming)));
        assert!(
            text.starts_with("\"Hangout\" (hs), 17:00-18:00 US/Eastern, at meeting-1@"),
            "{text}"
        );
        assert!(text.contains(
            "replaces entry 7's occurrences from Wed 2026-12-02 on (every 28 days until \
             2037-12-31), coming: Wed 2026-12-02, Wed 2026-12-30, Wed 2027-01-27, and 1 more\n"
        ));
        assert!(
            text.contains("  entry 7 becomes    Wed 2026-12-16 17:00 EST (22:00 UTC)\n"),
            "{text}"
        );
        assert!(text.contains(
            "new series         Wed 2027-01-20 17:00 EST (22:00 UTC), then every 28 days to \
             Wed 2027-03-17, 3 times"
        ));
        assert!(text.contains("    meeting-1 taken on Wed 2027-02-17"));
        assert!(text.contains("already booked, left alone: Wed 2027-04-21"));

        // A time a daylight-saving change skips is pointed out.
        let mut gap = t.clone();
        gap.start = NaiveTime::from_hms_opt(2, 30, 0).unwrap();
        let (planned, _) = plan(&gap, &[day("2027-03-14")], &BTreeSet::new(), false, 7, &[]);
        assert_eq!(
            planned[0].warnings,
            [
                "Sun 2027-03-14 02:30 does not exist in US/Eastern: a daylight-saving change skips it"
            ]
        );

        // Without --replace every entry is new; no location, no room.
        let t = Template::of(&form("", false)).unwrap();
        let (planned, _) = plan(&t, &days[..1], &BTreeSet::new(), false, 7, &bookings);
        assert_eq!(planned[0].action, "added");
        let text = describe(&t, &planned, &[], None);
        assert!(text.contains("no location"));
        assert!(text.contains("new entry"));
        assert!(!text.contains("replaces"));
    }

    /// A login that leaves a session on `uri`, as if the ticket worked.
    fn fake_login(uri: String) -> impl FnOnce() -> Result<Session, sandogasa_fedocal::Error> {
        move || {
            let host = uri
                .trim_start_matches("http://")
                .split(':')
                .next()
                .unwrap()
                .to_string();
            let jar = format!("{host}\tFALSE\t/\tFALSE\t0\tfedocal\ts\n");
            Session::login_with(&uri, move |argv| {
                let at = argv.iter().position(|a| a == "--cookie-jar").unwrap() + 1;
                std::fs::write(&argv[at], jar).unwrap();
                Ok(format!(
                    "{}/",
                    argv.last().unwrap().trim_end_matches("login/")
                ))
            })
        }
    }

    /// Entry 7's edit page: the hangout, every 28 days or not at all.
    fn page(recurring: bool) -> String {
        let (freq, buttons) = if recurring {
            (
                "<option selected value=\"28\">4 weeks</option>",
                "<input name=\"action\" type=\"submit\" value=\"Save\">\
                 <input name=\"action\" type=\"submit\" value=\"Save all\">",
            )
        } else {
            ("", "<input name=\"action\" type=\"submit\" value=\"Save\">")
        };
        format!(
            "<form action=\"\" method=\"post\">\
             <input name=\"calendar_name\" type=\"text\" value=\"hs\">\
             <input name=\"meeting_name\" type=\"text\" value=\"Hangout\">\
             <input name=\"meeting_date\" type=\"date\" value=\"2026-12-02\">\
             <input name=\"meeting_date_end\" type=\"date\" value=\"2026-12-02\">\
             <input name=\"meeting_time_start\" type=\"text\" value=\"17:00\">\
             <input name=\"meeting_time_stop\" type=\"text\" value=\"18:00\">\
             <input name=\"meeting_timezone\" type=\"text\" value=\"US/Eastern\">\
             <input name=\"meeting_location\" type=\"text\" value=\"https://meet.example/x\">\
             <select name=\"frequency\"><option value=\"\"></option>{freq}</select>\
             <input name=\"end_repeats\" type=\"date\" value=\"2037-12-31\">\
             <input name=\"csrf_token\" type=\"hidden\" value=\"t\">{buttons}</form>"
        )
    }

    fn api(meetings: &[(u64, &str, &str)]) -> ResponseTemplate {
        let list: Vec<serde_json::Value> = meetings
            .iter()
            .map(|(id, name, utc)| {
                let (date, time) = utc.split_once(' ').unwrap();
                serde_json::json!({
                    "meeting_id": id, "calendar_name": "hs", "meeting_name": name,
                    "meeting_date": date, "meeting_date_end": date,
                    "meeting_time_start": format!("{time}:00"),
                    "meeting_time_stop": "23:00:00", "meeting_timezone": "US/Eastern",
                    "meeting_location": "https://meet.example/x"
                })
            })
            .collect();
        ResponseTemplate::new(200)
            .set_body_json(serde_json::json!({"arguments": {}, "meetings": list}))
    }

    fn args(uri: &str, replace: bool) -> AddArgs {
        AddArgs {
            like: format!("{uri}/meeting/7/"),
            meeting: None,
            monthly: third_wed(),
            end: End::Count(5),
            replace,
            dry_run: false,
            yes: false,
            json: false,
        }
    }

    fn now() -> DateTime<Utc> {
        "2026-11-22T12:00:00Z".parse().unwrap()
    }

    /// Matches a request whose body ends with the text given.
    struct EndsWith(&'static str);

    impl wiremock::Match for EndsWith {
        fn matches(&self, request: &wiremock::Request) -> bool {
            request.body.ends_with(self.0.as_bytes())
        }
    }

    fn no_question(_: &str, _: bool) -> Result<bool, String> {
        panic!("no question expected")
    }

    #[tokio::test]
    async fn replace_turns_the_series_into_the_first_entry_and_adds_the_rest() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/meeting/edit/7/"))
            .respond_with(ResponseTemplate::new(200).set_body_string(page(true)))
            .mount(&server)
            .await;
        // Before: the series, and the April hangout booked already.
        Mock::given(method("GET"))
            .and(path("/api/meetings/"))
            .respond_with(api(&[
                (7, "Hangout", "2026-12-02 22:00"),
                (7, "Hangout", "2026-12-30 22:00"),
                (9, "Hangout", "2027-04-21 21:00"),
            ]))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        // After: every day asked for.
        Mock::given(method("GET"))
            .and(path("/api/meetings/"))
            .respond_with(api(&[
                (7, "Hangout", "2026-12-16 22:00"),
                (8, "Hangout", "2027-01-20 22:00"),
                (8, "Hangout", "2027-02-17 22:00"),
                (8, "Hangout", "2027-03-17 21:00"),
                (9, "Hangout", "2027-04-21 21:00"),
            ]))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/meeting/edit/7/"))
            .and(body_string_contains("action=Save+all"))
            .and(body_string_contains("frequency=&end_repeats=&"))
            .and(body_string_contains("meeting_date=2026-12-02"))
            .respond_with(ResponseTemplate::new(302).insert_header("location", "/meeting/7/"))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/meeting/edit/7/"))
            .and(body_string_contains("meeting_date=2026-12-16"))
            .and(EndsWith("action=Save"))
            .respond_with(ResponseTemplate::new(302).insert_header("location", "/meeting/7/"))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/hs/add/"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string("<input name=\"csrf_token\" type=\"hidden\" value=\"a1\">"),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/hs/add/"))
            .and(body_string_contains("csrf_token=a1"))
            .and(body_string_contains("meeting_date=2027-01-20"))
            .and(body_string_contains("frequency=28&end_repeats=2027-03-17"))
            .respond_with(ResponseTemplate::new(302).insert_header("location", "/hs/2027/1/20/"))
            .expect(1)
            .mount(&server)
            .await;
        let client = Client::with_base_url(&server.uri());
        let yes = |q: &str, default: bool| {
            assert_eq!(q, "Make these 2 change(s)?");
            assert!(!default);
            Ok(true)
        };
        let a = args(&server.uri(), true);
        let done = run(&client, &a, now(), yes, fake_login(server.uri()))
            .await
            .unwrap();
        assert_eq!(done.outcome, "booked");
        assert_eq!(done.calendar, "hs");
        assert_eq!(done.skipped, [day("2027-04-21")]);
        assert_eq!(done.entries.len(), 2);
        assert_eq!(done.entries[0].action, "moved");
        assert_eq!(done.entries[1].run.occurrences, 3);
        let json = serde_json::to_value(&done).unwrap();
        assert_eq!(json["entries"][1]["first"], "2027-01-20");
        assert_eq!(json["entries"][1]["action"], "added");
    }

    #[tokio::test]
    async fn a_dry_run_or_a_no_writes_nothing() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/meeting/edit/7/"))
            .respond_with(ResponseTemplate::new(200).set_body_string(page(true)))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/meetings/"))
            .respond_with(api(&[(7, "Hangout", "2026-12-02 22:00")]))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/hs/add/"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string("<input name=\"csrf_token\" type=\"hidden\" value=\"a1\">"),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&server)
            .await;
        let client = Client::with_base_url(&server.uri());
        let mut a = args(&server.uri(), true);
        a.dry_run = true;
        let done = run(&client, &a, now(), no_question, fake_login(server.uri()))
            .await
            .unwrap();
        assert_eq!(done.outcome, "dry_run");
        assert_eq!(done.entries.len(), 3);
        a.dry_run = false;
        let no = |_: &str, _: bool| Ok(false);
        let done = run(&client, &a, now(), no, fake_login(server.uri()))
            .await
            .unwrap();
        assert_eq!(done.outcome, "declined");
    }

    /// Entry `id`'s edit page from a series' page: its next day and end.
    fn series_page(next: &str, until: &str) -> String {
        page(true)
            .replace("value=\"2026-12-02\"", &format!("value=\"{next}\""))
            .replace("2037-12-31", until)
    }

    fn add_form() -> ResponseTemplate {
        ResponseTemplate::new(200)
            .set_body_string("<input name=\"csrf_token\" type=\"hidden\" value=\"a1\">")
    }

    #[tokio::test]
    async fn a_series_is_copied_only_when_it_is_one_of_the_bookings_runs() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/meeting/edit/7/"))
            .respond_with(ResponseTemplate::new(200).set_body_string(page(true)))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/meeting/edit/10/"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(series_page("2027-01-20", "2027-03-17")),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/meetings/"))
            .respond_with(api(&[
                (7, "Hangout", "2026-12-02 22:00"),
                (7, "Hangout", "2026-12-30 22:00"),
                (10, "Hangout", "2027-01-20 22:00"),
                (10, "Hangout", "2027-02-17 22:00"),
                (10, "Hangout", "2027-03-17 21:00"),
            ]))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/hs/add/"))
            .respond_with(add_form())
            .mount(&server)
            .await;
        let client = Client::with_base_url(&server.uri());
        // The 28-day series on the wrong days stays refused.
        let err = run(
            &client,
            &args(&server.uri(), false),
            now(),
            no_question,
            fake_login(server.uri()),
        )
        .await
        .unwrap_err();
        assert!(err.contains("add --replace"), "{err}");
        // A run an earlier booking made is the meeting itself: its days
        // are left alone and the rest booked beside it.
        let mut a = args(&server.uri(), false);
        a.like = format!("{}/meeting/10/", server.uri());
        a.dry_run = true;
        let done = run(&client, &a, now(), no_question, fake_login(server.uri()))
            .await
            .unwrap();
        assert_eq!(done.outcome, "dry_run");
        assert_eq!(
            done.skipped,
            [day("2027-01-20"), day("2027-02-17"), day("2027-03-17")]
        );
        let firsts: Vec<NaiveDate> = done.entries.iter().map(|e| e.run.first).collect();
        assert_eq!(firsts, [day("2026-12-16"), day("2027-04-21")]);
        // A run that goes on past the booking is not one of its runs.
        a.end = End::Count(3);
        let err = run(&client, &a, now(), no_question, fake_login(server.uri()))
            .await
            .unwrap_err();
        assert!(err.contains("add --replace"), "{err}");
    }

    #[tokio::test]
    async fn replace_moves_an_entry_that_does_not_repeat() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/meeting/edit/8/"))
            .respond_with(ResponseTemplate::new(200).set_body_string(page(false)))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/meetings/"))
            .respond_with(api(&[(8, "Hangout", "2026-12-02 22:00")]))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/meetings/"))
            .respond_with(api(&[(8, "Hangout", "2026-12-16 22:00")]))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/meeting/edit/8/"))
            .and(body_string_contains("action=Save+all"))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/meeting/edit/8/"))
            .and(body_string_contains("meeting_date=2026-12-16"))
            .and(EndsWith("action=Save"))
            .respond_with(ResponseTemplate::new(302).insert_header("location", "/meeting/8/"))
            .expect(1)
            .mount(&server)
            .await;
        let mut a = args(&server.uri(), true);
        a.like = format!("{}/meeting/8/", server.uri());
        a.end = End::Count(1);
        a.yes = true;
        let done = run(
            &Client::with_base_url(&server.uri()),
            &a,
            now(),
            no_question,
            fake_login(server.uri()),
        )
        .await
        .unwrap();
        assert_eq!(done.outcome, "booked");
        assert_eq!(done.entries[0].action, "moved");
    }

    #[tokio::test]
    async fn a_meeting_already_begun_today_is_not_booked() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/meeting/edit/8/"))
            .respond_with(ResponseTemplate::new(200).set_body_string(page(false)))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/meetings/"))
            .respond_with(api(&[]))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/hs/add/"))
            .respond_with(add_form())
            .mount(&server)
            .await;
        let mut a = args(&server.uri(), false);
        a.like = format!("{}/meeting/8/", server.uri());
        a.end = End::For("5w".into());
        a.dry_run = true;
        // 18:00 in New York on the third Wednesday: the 17:00 meeting has
        // begun, so the booking starts with January's.
        let evening = "2026-12-16T23:00:00Z".parse().unwrap();
        let done = run(
            &Client::with_base_url(&server.uri()),
            &a,
            evening,
            no_question,
            fake_login(server.uri()),
        )
        .await
        .unwrap();
        let firsts: Vec<NaiveDate> = done.entries.iter().map(|e| e.run.first).collect();
        assert_eq!(firsts, [day("2027-01-20")]);
    }

    #[tokio::test]
    async fn days_booked_already_are_left_alone() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/meeting/edit/7/"))
            .respond_with(ResponseTemplate::new(200).set_body_string(page(true)))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/meeting/edit/8/"))
            .respond_with(ResponseTemplate::new(200).set_body_string(page(false)))
            .mount(&server)
            .await;
        // Another entry holds the only day asked for.
        Mock::given(method("GET"))
            .and(path("/api/meetings/"))
            .respond_with(api(&[(9, "Hangout", "2026-12-16 22:00")]))
            .mount(&server)
            .await;
        let client = Client::with_base_url(&server.uri());
        let mut a = args(&server.uri(), false);
        a.like = format!("{}/meeting/8/", server.uri());
        a.end = End::Count(1);
        let done = run(&client, &a, now(), no_question, fake_login(server.uri()))
            .await
            .unwrap();
        assert_eq!(done.outcome, "unchanged");
        assert_eq!(done.skipped, [day("2026-12-16")]);
        // Replacing a series with nothing would only end it.
        let mut a = args(&server.uri(), true);
        a.end = End::Count(1);
        let err = run(&client, &a, now(), no_question, fake_login(server.uri()))
            .await
            .unwrap_err();
        assert!(err.contains("timebook edit 7 --until"), "{err}");
    }

    #[tokio::test]
    async fn a_failed_write_says_what_was_booked_and_how_to_go_on() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/meeting/edit/8/"))
            .respond_with(ResponseTemplate::new(200).set_body_string(page(false)))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/meetings/"))
            .respond_with(api(&[]))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/hs/add/"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string("<input name=\"csrf_token\" type=\"hidden\" value=\"a1\">"),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/hs/add/"))
            .and(body_string_contains("meeting_date=2026-12-16"))
            .respond_with(ResponseTemplate::new(302).insert_header("location", "/hs/2026/12/16/"))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/hs/add/"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        let client = Client::with_base_url(&server.uri());
        let mut a = args(&server.uri(), false);
        a.like = format!("{}/meeting/8/", server.uri());
        a.end = End::Count(2);
        a.yes = true;
        let err = run(&client, &a, now(), no_question, fake_login(server.uri()))
            .await
            .unwrap_err();
        assert!(err.contains("cannot add 2027-01-20"), "{err}");
        assert!(err.contains("booked already: 2026-12-16"), "{err}");
        assert!(err.contains("running the same command again"), "{err}");
    }

    #[tokio::test]
    async fn a_replace_stopped_halfway_says_how_to_finish() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/meeting/edit/7/"))
            .respond_with(ResponseTemplate::new(200).set_body_string(page(true)))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/meetings/"))
            .respond_with(api(&[(7, "Hangout", "2026-12-02 22:00")]))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/meeting/edit/7/"))
            .and(body_string_contains("action=Save+all"))
            .respond_with(ResponseTemplate::new(302).insert_header("location", "/meeting/7/"))
            .mount(&server)
            .await;
        let client = Client::with_base_url(&server.uri());
        let mut a = args(&server.uri(), true);
        a.yes = true;
        // The move fails: the entry stays where it was, single.
        let err = run(&client, &a, now(), no_question, fake_login(server.uri()))
            .await
            .unwrap_err();
        assert!(
            err.contains("stays on 2026-12-02 instead of moving to 2026-12-16"),
            "{err}"
        );
        assert!(err.contains("the same command again moves it"), "{err}");
        // The move works, an add fails: go on without --replace.
        Mock::given(method("POST"))
            .and(path("/meeting/edit/7/"))
            .and(EndsWith("action=Save"))
            .respond_with(ResponseTemplate::new(302).insert_header("location", "/meeting/7/"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/hs/add/"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;
        let err = run(&client, &a, now(), no_question, fake_login(server.uri()))
            .await
            .unwrap_err();
        assert!(err.contains("booked already: 2026-12-16"), "{err}");
        assert!(err.contains("again without --replace"), "{err}");
        // The series cannot be ended: nothing changed, the same command
        // is the way on.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/meeting/edit/7/"))
            .respond_with(ResponseTemplate::new(200).set_body_string(page(true)))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/meetings/"))
            .respond_with(api(&[]))
            .mount(&server)
            .await;
        let mut a = args(&server.uri(), true);
        a.yes = true;
        let err = run(
            &Client::with_base_url(&server.uri()),
            &a,
            now(),
            no_question,
            fake_login(server.uri()),
        )
        .await
        .unwrap_err();
        assert!(err.contains("cannot end the series"), "{err}");
        assert!(err.contains("the same command again"), "{err}");
    }

    #[tokio::test]
    async fn a_booking_the_calendar_does_not_show_is_an_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/meeting/edit/8/"))
            .respond_with(ResponseTemplate::new(200).set_body_string(page(false)))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/meetings/"))
            .respond_with(api(&[]))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/hs/add/"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string("<input name=\"csrf_token\" type=\"hidden\" value=\"a1\">"),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/hs/add/"))
            .respond_with(ResponseTemplate::new(302).insert_header("location", "/hs/2026/12/16/"))
            .mount(&server)
            .await;
        let client = Client::with_base_url(&server.uri());
        let mut a = args(&server.uri(), false);
        a.like = format!("{}/meeting/8/", server.uri());
        a.end = End::Count(1);
        a.yes = true;
        let err = run(&client, &a, now(), no_question, fake_login(server.uri()))
            .await
            .unwrap_err();
        assert!(err.contains("does not show 2026-12-16"), "{err}");
    }
}
