# timebook

Fedora's shared meeting rooms (`meeting`, `meeting-1`, ...) as Fedora
Calendar books them: which rooms a planned meeting, or a series of
them, would find free, and what each room holds.

A calendar entry's location is free text, so one room appears under
many names: `meeting-1@chat.fedoraproject.org`,
`https://matrix.to/#/%23meeting-1:fedoraproject.org`, and the IRC
channel it replaced, `fedora-meeting-1@irc.libera.chat`. timebook reads
them all as the room; a video call link or a city is not a shared room.
Times are compared in UTC.

## Installation

```
cargo install timebook
```

## Usage

### cancel

Takes occurrences of one of your own meetings off the calendar, past or
coming: a meeting that was not held, or will not be, so the calendar
says what happened and the room shows as free. The meeting is given by
its calendar, with `--meeting` and part of its title when the calendar
holds other meetings on those days, or by the id or page of any of its
entries; the days are the meeting's own dates.

```
$ timebook cancel centos-sig-hyperscale --meeting "CentOS Hyperscale SIG" 2026-10-21 2026-11-04
To cancel in centos-sig-hyperscale, "CentOS Hyperscale SIG":
  Wed 2026-10-21 15:00 UTC (entry 11067)
  Wed 2026-11-04 16:00 UTC (entry 11067)
Cancel these 2 occurrence(s)? [y/N]:
```

fedocal cancels one occurrence of a series by ending the series the day
before and starting it again, as a new entry, after it; `cancel` finds
each occurrence by the meeting's calendar and title, whichever entry
holds it, and checks after each that it is gone. It logs in with your
Kerberos ticket (`fkinit`); `--dry-run` lists the occurrences and checks
you may cancel them, `--yes` cancels without asking, and `--json` (with
either) prints the outcome.

### check

Whether a room is free for a planned meeting or series, and if not,
what it clashes with:

```
$ timebook check meeting "2026-12-01 18:00" --tz Europe/London --every 7 --count 3
  Tue 2026-12-01 18:00 GMT (18:00 UTC)  taken: FESCo Meeting (fesco) 18:00-20:00 UTC
  Tue 2026-12-08 18:00 GMT (18:00 UTC)  taken: FESCo Meeting (fesco) 18:00-20:00 UTC
  Tue 2026-12-15 18:00 GMT (18:00 UTC)  taken: FESCo Meeting (fesco) 18:00-20:00 UTC
meeting is taken in 3 of 3.
```

The room is given by any of its names, and the start time
(`YYYY-MM-DD HH:MM`) in `--tz` (default UTC); a series keeps its local
time across a daylight-saving change. `--minutes` sets each meeting's
length (default 60, up to 1440), `--every` the days between them
(default 7, up to 366) and `--count` how many (default 1, up to 520).

### edit

Changes one of your own entries: its title (`--title`), location
(`--location`), description (`--note`) or, for a recurring entry, the
last day it repeats on (`--until`). The entry is given by its id or its
page, or by its meeting's calendar (with `--meeting` and part of its
title when the calendar holds several), for the entry of the meeting's
next occurrence in the coming 92 days. Fedora Calendar has no API for
changing entries, so `edit` logs in to it as a browser would, with your
Kerberos ticket (`fkinit`), reads the entry, shows each change before
and after, saves once you confirm, and reads it back; everything else in
the entry is kept.

```
$ timebook edit https://calendar.fedoraproject.org/meeting/11171/ --location "https://meet.example/hangout"
Changes to https://calendar.fedoraproject.org/meeting/11171/:
  covers:    the series from its next occurrence, 2026-12-02, on; fedocal keeps the earlier ones as a separate entry
  location:  (none) -> "https://meet.example/hangout"
Save these changes? [Y/n]:
```

For a recurring entry the whole series changes from its next
occurrence on, as the calendar's "Save all" does, and the occurrences
before it stay as they were, as a separate entry; `--one` changes the
next occurrence alone, the series going on without it. A video
meeting's public link as its location lets `stale` tell it is a video
call. `--dry-run` logs in and shows the changes; `--yes` saves without
asking, and `--json` (with either) prints the outcome.

### find

The same planned meeting or series against every room, the free ones
first:

```
$ timebook find "2026-12-02 16:00" --tz Europe/Dublin --every 14 --count 6
6 meeting(s) from Wed 2026-12-02 16:00 GMT (16:00 UTC):
  meeting-1  free for all 6
  meeting-2  free for all 6
  meeting-3  free for all 6
  meeting    taken in 3 of 6: CentOS Hyperscale SIG (centos-sig-hyperscale)
```

When no room is free for every meeting, or with `--suggest` (which
`check` takes too, for its room), the meetings in the taken rooms are
looked up in meetbot as `stale` does, and a room whose meetings have
had no minutes for any of their occurrences in the last 12 weeks is
listed as likely free, with the evidence. An entry with no occurrence
in that time is judged through the earlier entries of the same
calendar and title, since teams recreate entries; one that held even
one meeting there is not judged, and neither is a meeting whose
meetbot search failed:

```
$ timebook find "2026-11-24 10:00" --minutes 780 --count 4 --suggest
4 meeting(s) from Tue 2026-11-24 10:00 UTC:
  meeting-2  free for all 4
  meeting-3  free for all 4
  meeting-1  taken in 3 of 4: ELN SIG (SIGs)
             likely free all the same:
               ELN SIG (SIGs): no minutes for any of 11 occurrence(s) since 2026-09-01, judged by its earlier entry 11388, 11456 (searched meetbot for: sigs, eln-sig, fedora-eln-sig)
  meeting    taken in 4 of 4: FESCo Meeting (fesco), Fedora Docs Team Meeting (docs)
```

A room taken by a meeting that stopped happening is only likely free:
its owners may still mean to hold it, which `stale` helps find out.

### rooms

What each room holds in a period (`--from`, default today, and
`--days`, default 28), one line per series:

```
$ timebook rooms
Rooms from 2026-11-19 to 2026-12-16:

meeting
  Mon Fedora QA Meeting (QA) 16:00-17:00 UTC  2 times
  Tue Fedora Docs Team Meeting (docs) 13:00-14:00 UTC  2 times
  Tue FESCo Meeting (fesco) 18:00-20:00 UTC  4 times
  Wed Fedora Council Meeting (council) 15:00-16:00 UTC  2 times
  Wed CentOS Hyperscale SIG (centos-sig-hyperscale) 16:00-17:00 UTC  2 times
  ...
```

### stale

Whether the recurring meetings of the last weeks (`--weeks`, default 12)
still take place. meetbot keeps minutes by topic, so each series is
looked for under its calendar's name and its title as a topic (`fesco`,
`fedora-eln-sig`); an occurrence counts as held when minutes start
within an hour of it, preferably in the room it is booked in. Series are
grouped as held, rarely held, without minutes, unchecked (a meetbot
search failed, so nothing can be said), and video calls (which meetbot
does not log); minutes found in another room than the one booked are
pointed out, and for a series without minutes the owners' last activity
on Discourse is shown. For a series held at least sometimes whose
searches all answered, the days without minutes are listed with the
`timebook cancel` command that takes them off the calendar, for its
owners to run if those meetings did not take place; occurrences of the
last day are left out unless `--refresh` is given, since meetbot's
answers are kept for a day. The topics searched are listed, so a series
filed under another topic can be checked by hand.

```
$ timebook stale
No minutes found (14):
  Fedora Packaging Committee (packaging): Thu 17:00 UTC, no location, 6 occurrence(s)
    searched meetbot for: packaging, fedora-packaging-committee
    owners: james (seen 2026-09-23), pingou (seen 2019-06-08), ...
...
Held (14):
  Fedora Infrastructure (infrastructure): Thu 16:00 UTC, in meeting-3, 13 occurrence(s)
    held 13 of 13, last https://meetbot.fedoraproject.org/...
    minutes are in meeting, not the room booked
  FESCo Meeting (fesco): Tue 18:00 UTC, in meeting, 12 occurrence(s)
    held 11 of 12, last https://meetbot.fedoraproject.org/...
    no minutes on 2026-10-27; if not held, the owners can take them off with
      timebook cancel fesco --meeting 'FESCo Meeting' 2026-10-27
...
Stopped at 2025-12-31, fedocal's old default end for a series without one (19):
  Fedora Infra Ops Daily Standup (infrastructure): Wed 19:00 UTC, fedora-admin@irc.libera.chat, 5 occurrence(s)
    still held: minutes on 2026-11-18 (https://meetbot.fedoraproject.org/...); its entry needs a new end
```

The last group is series that ended at 2025-12-31, fedocal's default
end for a series created without one until May 2026, and were not
replaced by a new entry: those with minutes since, at their usual time,
are still held but no longer on the calendar. meetbot's answers are
kept for a day; `--refresh` asks again.

`--json` prints any of them as JSON.

## License

Licensed under either of

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT License](LICENSE-MIT)

at your option.
