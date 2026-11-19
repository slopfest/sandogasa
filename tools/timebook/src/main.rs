// SPDX-License-Identifier: Apache-2.0 OR MIT

use std::collections::BTreeMap;
use std::process::ExitCode;

use chrono::{Duration, NaiveDate, NaiveDateTime, Utc};
use chrono_tz::Tz;
use clap::{Args, Parser, Subcommand};
use sandogasa_fedocal::{Client, MeetingQuery};

mod book;

use book::{Booking, RoomCheck, Slot};

/// Fedora's shared meeting rooms (meeting, meeting-1, ...) as Fedora
/// Calendar books them: which rooms a planned meeting, or a series of
/// them, would find free, and what each room holds.
///
/// A calendar entry's location is free text, so one room appears under
/// many names (meeting-1@chat.fedoraproject.org, a matrix.to link, the
/// IRC channel it replaced); timebook reads them all as the room. Times
/// are compared in UTC.
#[derive(Parser)]
#[command(
    about,
    long_about,
    max_term_width = 80,
    version = sandogasa_cli::version!(),
    before_help = sandogasa_cli::banner!()
)]
struct Cli {
    /// Print JSON instead of text
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Check whether a room is free for a planned meeting or series
    Check {
        /// The room: meeting, meeting-1, or any name of it
        room: String,

        #[command(flatten)]
        series: SeriesArgs,
    },

    /// Find the rooms free for a planned meeting or series
    Find {
        #[command(flatten)]
        series: SeriesArgs,
    },

    /// List what each room holds in a period
    Rooms {
        /// Days to cover (1 to 366)
        #[arg(long, default_value_t = 28, value_parser = clap::value_parser!(i64).range(1..=366))]
        days: i64,

        /// First day (YYYY-MM-DD; default today)
        #[arg(long)]
        from: Option<NaiveDate>,
    },
}

/// A planned meeting, and how often it repeats.
#[derive(Args)]
struct SeriesArgs {
    /// When the first one starts: YYYY-MM-DD HH:MM, in --tz
    at: String,

    /// How many meetings in the series (1 to 520)
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u64).range(1..=520))]
    count: u64,

    /// Days between meetings (1 to 366)
    #[arg(long, default_value_t = 7, value_parser = clap::value_parser!(i64).range(1..=366))]
    every: i64,

    /// How long each meeting is, in minutes (1 to 1440)
    #[arg(long, default_value_t = 60, value_parser = clap::value_parser!(i64).range(1..=1440))]
    minutes: i64,

    /// The time zone of --at, e.g. Europe/Dublin
    #[arg(long, default_value = "UTC")]
    tz: String,
}

fn to_json<T: serde::Serialize>(value: &T) -> Result<String, String> {
    serde_json::to_string_pretty(value).map_err(|e| e.to_string())
}

/// A slot's start in the planned time zone, with UTC beside it.
fn when(slot: &Slot, tz: Tz) -> String {
    let local = slot.start.with_timezone(&tz);
    if tz == chrono_tz::UTC {
        local.format("%a %Y-%m-%d %H:%M UTC").to_string()
    } else {
        format!(
            "{} ({})",
            local.format("%a %Y-%m-%d %H:%M %Z"),
            slot.start.format("%H:%M UTC")
        )
    }
}

/// A booking for a list: name, calendar and time in UTC.
fn booking_line(b: &Booking) -> String {
    format!(
        "{} ({}) {}-{} UTC",
        b.name,
        b.calendar,
        b.start.format("%H:%M"),
        b.end.format("%H:%M")
    )
}

fn describe_check(result: &RoomCheck, tz: Tz) -> String {
    let mut out = String::new();
    for s in &result.slots {
        let state = if s.clashes.is_empty() {
            "free".to_string()
        } else {
            let names: Vec<String> = s.clashes.iter().map(booking_line).collect();
            format!("taken: {}", names.join("; "))
        };
        out.push_str(&format!("  {}  {state}\n", when(&s.slot, tz)));
    }
    let (taken, n) = (result.taken(), result.slots.len());
    out.push_str(&if taken == 0 {
        format!("{} is free for all {n}.\n", result.room)
    } else {
        format!("{} is taken in {taken} of {n}.\n", result.room)
    });
    out
}

fn describe_find(results: &[RoomCheck], slots: &[Slot], tz: Tz) -> String {
    let n = slots.len();
    let mut out = format!(
        "{n} meeting(s) from {}:\n",
        slots.first().map(|s| when(s, tz)).unwrap_or_default()
    );
    for r in results {
        let taken = r.taken();
        if taken == 0 {
            out.push_str(&format!("  {:<10} free for all {n}\n", r.room));
            continue;
        }
        let mut names: Vec<String> = r
            .slots
            .iter()
            .flat_map(|s| {
                s.clashes
                    .iter()
                    .map(|b| format!("{} ({})", b.name, b.calendar))
            })
            .collect();
        names.sort();
        names.dedup();
        out.push_str(&format!(
            "  {:<10} taken in {taken} of {n}: {}\n",
            r.room,
            names.join(", ")
        ));
    }
    out
}

fn describe_rooms(
    by_room: &BTreeMap<String, Vec<Booking>>,
    from: NaiveDate,
    to: NaiveDate,
) -> String {
    let mut out = format!("Rooms from {from} to {to}:\n");
    for (room, bookings) in by_room {
        out.push_str(&format!("\n{room}\n"));
        if bookings.is_empty() {
            out.push_str("  nothing booked\n");
            continue;
        }
        // One line per series, at its first occurrence's time.
        let mut series: BTreeMap<u64, (&Booking, usize)> = BTreeMap::new();
        for b in bookings {
            series.entry(b.meeting_id).or_insert((b, 0)).1 += 1;
        }
        let mut lines: Vec<(&Booking, usize)> = series.into_values().collect();
        lines.sort_by_key(|(b, _)| (b.start.format("%u %H:%M").to_string(), b.meeting_id));
        for (b, times) in lines {
            let times = if times == 1 {
                "once".to_string()
            } else {
                format!("{times} times")
            };
            out.push_str(&format!(
                "  {} {}  {times}\n",
                b.start.format("%a"),
                booking_line(b)
            ));
        }
    }
    out
}

/// The slots `args` plan, and the time zone they are given in.
fn planned(args: &SeriesArgs) -> Result<(Vec<Slot>, Tz), String> {
    let tz: Tz = args
        .tz
        .parse()
        .map_err(|_| format!("`{}` is not a time zone (e.g. Europe/Dublin)", args.tz))?;
    let first = NaiveDateTime::parse_from_str(&args.at, "%Y-%m-%d %H:%M")
        .map_err(|_| format!("`{}` is not a time: give YYYY-MM-DD HH:MM", args.at))?;
    // clap keeps --count, --every and --minutes in range; checked again
    // here for callers that build the arguments themselves.
    if !(1..=520).contains(&args.count)
        || !(1..=1440).contains(&args.minutes)
        || !(1..=366).contains(&args.every)
    {
        return Err("--count, --minutes or --every is out of range".into());
    }
    let slots = book::series(first, tz, args.minutes, args.every, args.count as usize);
    if slots.is_empty() {
        return Err(format!(
            "{} does not exist in {tz} (a daylight-saving gap)",
            args.at
        ));
    }
    Ok((slots, tz))
}

/// The room bookings from a day before the first slot to a day after the
/// last, in one request.
async fn fetch_around(client: &Client, slots: &[Slot]) -> Result<Vec<Booking>, String> {
    let first = slots.iter().map(|s| s.start).min().unwrap_or_default();
    let last = slots.iter().map(|s| s.end).max().unwrap_or_default();
    let (from, to) = (
        first.date_naive() - Duration::days(1),
        last.date_naive() + Duration::days(1),
    );
    eprintln!("Reading Fedora Calendar from {from} to {to}...");
    let meetings = client
        .meetings(&MeetingQuery::default().between(from, to))
        .await
        .map_err(|e| e.to_string())?;
    Ok(book::bookings(&meetings))
}

/// `check`: one room against the planned series.
async fn check(client: &Client, room: &str, args: &SeriesArgs, json: bool) -> Result<(), String> {
    let room = book::room_named(room)?;
    let (slots, tz) = planned(args)?;
    let bookings = fetch_around(client, &slots).await?;
    let result = book::check(&room, &slots, &bookings);
    if json {
        println!("{}", to_json(&result)?);
        return Ok(());
    }
    print!("{}", describe_check(&result, tz));
    Ok(())
}

/// `find`: every room against the planned series, the free ones first.
async fn find(client: &Client, args: &SeriesArgs, json: bool) -> Result<(), String> {
    let (slots, tz) = planned(args)?;
    let bookings = fetch_around(client, &slots).await?;
    let mut results: Vec<RoomCheck> = book::rooms(&bookings)
        .iter()
        .map(|room| book::check(room, &slots, &bookings))
        .collect();
    results.sort_by_key(|r| r.taken());
    if json {
        println!("{}", to_json(&results)?);
        return Ok(());
    }
    print!("{}", describe_find(&results, &slots, tz));
    Ok(())
}

/// `rooms`: each room's bookings in `days` from `from`, by series.
async fn rooms(client: &Client, from: NaiveDate, days: i64, json: bool) -> Result<(), String> {
    let to = from + Duration::days(days - 1);
    eprintln!("Reading Fedora Calendar from {from} to {to}...");
    let meetings = client
        .meetings(&MeetingQuery::default().between(from, to))
        .await
        .map_err(|e| e.to_string())?;
    let bookings = book::bookings(&meetings);
    let mut by_room: BTreeMap<String, Vec<Booking>> = book::rooms(&bookings)
        .into_iter()
        .map(|r| (r, Vec::new()))
        .collect();
    for b in bookings {
        by_room.entry(b.room.clone()).or_default().push(b);
    }
    if json {
        println!("{}", to_json(&by_room)?);
        return Ok(());
    }
    print!("{}", describe_rooms(&by_room, from, to));
    Ok(())
}

#[tokio::main]
async fn main() -> ExitCode {
    sandogasa_cli::init();
    let cli = sandogasa_cli::parse_with_defaults::<Cli>(env!("CARGO_PKG_NAME"));
    let client = Client::new();
    let result = match &cli.command {
        Command::Check { room, series } => check(&client, room, series, cli.json).await,
        Command::Find { series } => find(&client, series, cli.json).await,
        Command::Rooms { days, from } => {
            let from = from.unwrap_or_else(|| Utc::now().date_naive());
            rooms(&client, from, *days, cli.json).await
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The committed man page is generated from this CLI; see
    /// `sandogasa_cli::man` and `scripts/gen-man.sh`.
    #[test]
    fn man_page_matches_cli() {
        sandogasa_cli::man::check::<super::Cli>(
            concat!(env!("CARGO_MANIFEST_DIR"), "/man/timebook.1"),
            env!("CARGO_PKG_VERSION"),
        );
    }

    fn args(at: &str, count: u64, tz: &str) -> SeriesArgs {
        SeriesArgs {
            at: at.into(),
            count,
            every: 14,
            minutes: 60,
            tz: tz.into(),
        }
    }

    #[test]
    fn a_planned_series_reads_or_says_why_not() {
        let (slots, tz) = planned(&args("2026-12-02 15:00", 3, "Europe/Dublin")).unwrap();
        assert_eq!(slots.len(), 3);
        assert_eq!(tz, chrono_tz::Europe::Dublin);
        assert!(
            planned(&args("2026-12-02 15:00", 1, "Mars/Olympus"))
                .unwrap_err()
                .contains("not a time zone")
        );
        assert!(
            planned(&args("tomorrow", 1, "UTC"))
                .unwrap_err()
                .contains("YYYY-MM-DD HH:MM")
        );
        assert!(
            planned(&args("2026-12-02 15:00", 0, "UTC"))
                .unwrap_err()
                .contains("out of range")
        );
        assert!(
            planned(&args("2026-03-29 01:30", 1, "Europe/Dublin"))
                .unwrap_err()
                .contains("gap")
        );
    }

    fn booking(id: u64, room: &str, start: &str, end: &str) -> Booking {
        let t = |s: &str| {
            NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M")
                .unwrap()
                .and_utc()
        };
        Booking {
            room: room.into(),
            start: t(start),
            end: t(end),
            meeting_id: id,
            name: format!("M{id}"),
            calendar: "cal".into(),
        }
    }

    #[test]
    fn checks_and_rooms_read_as_text() {
        let bookings = vec![
            booking(1, "meeting-1", "2026-12-02 15:00", "2026-12-02 16:00"),
            booking(1, "meeting-1", "2026-12-09 15:00", "2026-12-09 16:00"),
        ];
        let (slots, tz) = planned(&SeriesArgs {
            every: 7,
            ..args("2026-12-02 15:30", 2, "Europe/Dublin")
        })
        .unwrap();
        let one = book::check("meeting-1", &slots, &bookings);
        assert_eq!(
            describe_check(&one, tz),
            "  Wed 2026-12-02 15:30 GMT (15:30 UTC)  taken: M1 (cal) 15:00-16:00 UTC\n  \
             Wed 2026-12-09 15:30 GMT (15:30 UTC)  taken: M1 (cal) 15:00-16:00 UTC\n\
             meeting-1 is taken in 2 of 2.\n"
        );
        let free = book::check("meeting-2", &slots, &bookings);
        assert!(describe_check(&free, chrono_tz::UTC).ends_with("meeting-2 is free for all 2.\n"));
        let found = describe_find(&[free, one], &slots, tz);
        assert!(found.contains("  meeting-2  free for all 2\n"), "{found}");
        assert!(
            found.contains("  meeting-1  taken in 2 of 2: M1 (cal)\n"),
            "{found}"
        );
        let mut by_room = BTreeMap::new();
        by_room.insert("meeting".to_string(), vec![]);
        by_room.insert("meeting-1".to_string(), bookings);
        let day = |s| NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap();
        assert_eq!(
            describe_rooms(&by_room, day("2026-12-01"), day("2026-12-28")),
            "Rooms from 2026-12-01 to 2026-12-28:\n\nmeeting\n  nothing booked\n\n\
             meeting-1\n  Wed M1 (cal) 15:00-16:00 UTC  2 times\n"
        );
    }
}
