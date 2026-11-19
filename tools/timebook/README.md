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

Whether the recurring meetings of the last weeks (`--weeks`, default
12) still take place. meetbot keeps minutes by topic, so each series is
looked for under its calendar's name and its title as a topic (`fesco`,
`fedora-eln-sig`); an occurrence counts as held when minutes start
within an hour of it, preferably in the room it is booked in. Series
are grouped as held, rarely held, without minutes, and video calls
(which meetbot does not log); minutes found in another room than the
one booked are pointed out, and for a series without minutes the
owners' last activity on Discourse is shown. The topics searched are
listed, so a series filed under another topic can be checked by hand.

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
