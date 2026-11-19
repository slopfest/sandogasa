// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Bookings of Fedora's shared meeting rooms, and the slots a planned
//! meeting would take: the pure part of `timebook`, apart from fetching.

use std::collections::BTreeSet;

use chrono::{DateTime, Duration, NaiveDateTime, TimeZone, Utc};
use chrono_tz::Tz;
use sandogasa_fedocal::Meeting;
use sandogasa_fedocal::room::Room;
use serde::Serialize;

/// The rooms Fedora keeps for meetings, checked even when nothing in
/// the period is booked in them.
pub const ROOMS: [&str; 4] = ["meeting", "meeting-1", "meeting-2", "meeting-3"];

/// One occurrence of a calendar entry in a shared room.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Booking {
    pub room: String,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub meeting_id: u64,
    pub name: String,
    pub calendar: String,
}

/// A time a planned meeting would take.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Slot {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

/// What a planned series would hit in one room.
#[derive(Debug, Clone, Serialize)]
pub struct RoomCheck {
    pub room: String,
    /// Per slot, the bookings it clashes with (none: the room is free).
    pub slots: Vec<SlotCheck>,
}

/// One slot of a planned series, and what it clashes with.
#[derive(Debug, Clone, Serialize)]
pub struct SlotCheck {
    pub slot: Slot,
    pub clashes: Vec<Booking>,
}

impl RoomCheck {
    /// How many slots clash with something.
    pub fn taken(&self) -> usize {
        self.slots.iter().filter(|s| !s.clashes.is_empty()).count()
    }
}

/// `count` slots of `minutes` each, the first at `first` local time in
/// `tz` and every `every_days` after, keeping the local time across a
/// daylight-saving change. A local time that does not exist (in a
/// daylight-saving gap) is skipped.
pub fn series(
    first: NaiveDateTime,
    tz: Tz,
    minutes: i64,
    every_days: i64,
    count: usize,
) -> Vec<Slot> {
    (0..count)
        .filter_map(|i| {
            let local = first + Duration::days(every_days * i as i64);
            let start = tz
                .from_local_datetime(&local)
                .earliest()?
                .with_timezone(&Utc);
            Some(Slot {
                start,
                end: start + Duration::minutes(minutes),
            })
        })
        .collect()
}

/// The occurrences of `meetings` held in a shared room, each once,
/// earliest first. Entries without a readable time are left out.
pub fn bookings(meetings: &[Meeting]) -> Vec<Booking> {
    let mut out: Vec<Booking> = meetings
        .iter()
        .filter_map(|m| {
            Some(Booking {
                room: m.room()?.name,
                start: m.starts_at()?,
                end: m.ends_at()?,
                meeting_id: m.meeting_id,
                name: m.meeting_name.clone(),
                calendar: m.calendar_name.clone(),
            })
        })
        .collect();
    out.sort_by_key(|b| (b.start, b.meeting_id));
    out.dedup_by(|a, b| a.meeting_id == b.meeting_id && a.start == b.start);
    out
}

/// What `slots` would clash with in `room`: bookings overlapping a slot
/// (one ending as the other starts does not).
pub fn check(room: &str, slots: &[Slot], bookings: &[Booking]) -> RoomCheck {
    RoomCheck {
        room: room.to_string(),
        slots: slots
            .iter()
            .map(|slot| SlotCheck {
                slot: *slot,
                clashes: bookings
                    .iter()
                    .filter(|b| b.room == room && b.start < slot.end && slot.start < b.end)
                    .cloned()
                    .collect(),
            })
            .collect(),
    }
}

/// Every room to consider: Fedora's own, and any other shared room the
/// bookings use, in order.
pub fn rooms(bookings: &[Booking]) -> Vec<String> {
    let mut all: BTreeSet<String> = ROOMS.iter().map(|r| r.to_string()).collect();
    all.extend(bookings.iter().map(|b| b.room.clone()));
    all.into_iter().collect()
}

/// The room `name` names (`meeting-1`, `#meeting-1:fedoraproject.org`,
/// a matrix.to link...), or why not.
pub fn room_named(name: &str) -> Result<String, String> {
    Room::parse(name).map(|r| r.name).ok_or_else(|| {
        format!("`{name}` is not one of Fedora's meeting rooms (meeting, meeting-N)")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M").unwrap()
    }

    fn utc(s: &str) -> DateTime<Utc> {
        at(s).and_utc()
    }

    fn meeting(id: u64, date: &str, start: &str, stop: &str, tz: &str, location: &str) -> Meeting {
        serde_json::from_value(serde_json::json!({
            "meeting_id": id, "calendar_name": "cal", "meeting_name": format!("m{id}"),
            "meeting_date": date, "meeting_date_end": date,
            "meeting_time_start": start, "meeting_time_stop": stop,
            "meeting_timezone": tz, "meeting_location": location
        }))
        .unwrap()
    }

    #[test]
    fn a_series_keeps_its_local_time_across_daylight_saving() {
        // Dublin falls back on 2026-10-25: 15:00 local is 14:00 UTC
        // before and 15:00 UTC after.
        let slots = series(at("2026-10-14 15:00"), chrono_tz::Europe::Dublin, 60, 14, 2);
        assert_eq!(slots[0].start, utc("2026-10-14 14:00"));
        assert_eq!(slots[1].start, utc("2026-10-28 15:00"));
        assert_eq!(slots[1].end, utc("2026-10-28 16:00"));
        // A local time in the spring-forward gap is skipped.
        let gap = series(at("2026-03-29 01:30"), chrono_tz::Europe::Dublin, 30, 7, 1);
        assert!(gap.is_empty());
    }

    #[test]
    fn bookings_are_each_occurrence_in_a_shared_room_once() {
        let meetings = [
            meeting(
                2,
                "2026-12-02",
                "16:00:00",
                "17:00:00",
                "US/Eastern",
                "meeting@fedoraproject.org",
            ),
            meeting(
                1,
                "2026-12-02",
                "15:00:00",
                "16:00:00",
                "UTC",
                "meeting-1@chat.fedoraproject.org",
            ),
            meeting(
                1,
                "2026-12-02",
                "15:00:00",
                "16:00:00",
                "UTC",
                "meeting-1@chat.fedoraproject.org",
            ),
            meeting(
                3,
                "2026-12-02",
                "15:00:00",
                "16:00:00",
                "UTC",
                "https://meet.google.com/x",
            ),
            meeting(
                4,
                "2026-12-02",
                "15:00:00",
                "16:00:00",
                "Nowhere/Land",
                "meeting",
            ),
        ];
        let b = bookings(&meetings);
        let shown: Vec<String> = b
            .iter()
            .map(|b| format!("{} {} {}", b.meeting_id, b.room, b.start.format("%H:%M")))
            .collect();
        assert_eq!(shown, ["1 meeting-1 15:00", "2 meeting 16:00"]);
        assert_eq!(
            rooms(&b),
            ["meeting", "meeting-1", "meeting-2", "meeting-3"]
        );
    }

    #[test]
    fn a_slot_clashes_with_what_overlaps_it_in_the_room() {
        let b = bookings(&[
            meeting(
                1,
                "2026-12-02",
                "15:00:00",
                "16:00:00",
                "UTC",
                "meeting-1@chat.fedoraproject.org",
            ),
            meeting(
                2,
                "2026-12-09",
                "15:30:00",
                "16:30:00",
                "UTC",
                "meeting-1@chat.fedoraproject.org",
            ),
            meeting(
                3,
                "2026-12-16",
                "16:00:00",
                "17:00:00",
                "UTC",
                "meeting-1@chat.fedoraproject.org",
            ),
            meeting(
                4,
                "2026-12-02",
                "15:00:00",
                "16:00:00",
                "UTC",
                "meeting-2@chat.fedoraproject.org",
            ),
        ]);
        let slots = series(at("2026-12-02 15:00"), chrono_tz::UTC, 60, 7, 3);
        let one = check("meeting-1", &slots, &b);
        let ids: Vec<Vec<u64>> = one
            .slots
            .iter()
            .map(|s| s.clashes.iter().map(|c| c.meeting_id).collect())
            .collect();
        // The third ends as #3 starts: no clash.
        assert_eq!(ids, [vec![1], vec![2], vec![]]);
        assert_eq!(one.taken(), 2);
        assert_eq!(check("meeting-3", &slots, &b).taken(), 0);
    }

    #[test]
    fn rooms_are_named_as_people_write_them() {
        assert_eq!(room_named("meeting-1").unwrap(), "meeting-1");
        assert_eq!(
            room_named("https://matrix.to/#/%23meeting:fedoraproject.org").unwrap(),
            "meeting"
        );
        assert!(
            room_named("lobby")
                .unwrap_err()
                .contains("not one of Fedora's meeting rooms")
        );
    }
}
