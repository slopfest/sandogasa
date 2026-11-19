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

`--json` prints any of them as JSON.

## License

Licensed under either of

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT License](LICENSE-MIT)

at your option.
