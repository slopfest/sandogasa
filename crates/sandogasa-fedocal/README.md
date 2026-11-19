# sandogasa-fedocal

A client for [Fedora Calendar](https://calendar.fedoraproject.org/)
(fedocal): reading its calendars' entries through the public JSON API,
and adding entries as a logged-in user.

- `Client::meetings(query)` reads entries, filtered by calendar,
  location and date range (entries overlapping the range are
  returned).
- `Client::absences(user, from, to)` reads a user's entries in the
  vacation calendar.
- `Meeting::last_day` gives a full-day entry's last day: fedocal stores
  those from 00:00 to 00:00 UTC, ending on the day after.
  `Meeting::starts_at` and `ends_at` give an occurrence's times in UTC,
  as the API gives them; `meeting_timezone` names the time zone it was
  booked in. The one query shape that answers in local times, a
  calendar and a location together, is converted to UTC by
  `Client::meetings`.
- `room::Room` reads the free-text locations entries give as Fedora's
  shared meeting rooms (`meeting`, `meeting-N`):
  `meeting-1@chat.fedoraproject.org`, a matrix.to link, a Matrix alias,
  and the IRC channel the room replaced all read as `meeting-1`.
- `session::Session::kerberos(base_url)` logs in with the user's
  Kerberos ticket (`fkinit`). fedocal has no API for writing, and logs
  in through id.fedoraproject.org's OpenID Connect provider, which
  accepts a ticket; the login runs `curl --negotiate`, with
  `--location-trusted` so the ticket reaches the provider after
  fedocal's redirect, and keeps the session's cookies. It needs `curl`
  and `klist`. fedocal's session lasts an hour.
- `Session::can_add(calendar)` checks that the user may add to a
  calendar, and `Session::add(entry)` fills in and submits the add form,
  CSRF token included. A refusal carries fedocal's own messages.
- `Session::can_edit(id)` and `Session::edit(id, changes)` do the same
  for an existing entry, and `Session::entry(id)` reads it as its edit
  form shows it. fedocal replaces every field on saving, so `edit`
  reads the edit form as a browser would send it and changes only the
  title, days, information, location or series end asked for, keeping
  co-managers, reminders and the rest. For a recurring entry,
  `Changes::whole_series` saves the series from its next occurrence on
  ("Save all"), fedocal keeping the earlier ones as a separate entry;
  otherwise that occurrence alone is changed ("Save").
- `Session::cancel(id, date)` (and `can_cancel`) takes one occurrence of
  an entry off the calendar through fedocal's delete form, `date` being
  the occurrence's own day (`Meeting::local_date`); fedocal ends the
  series before it and starts it again, as a new entry, after it. Its
  answer is the same whether or not anything was deleted, so the caller
  reads the calendar back.

## Usage

```rust
use sandogasa_fedocal::session::{NewEntry, Session};
use sandogasa_fedocal::{Client, DEFAULT_BASE_URL, VACATION};

let client = Client::new();
let away = client.absences("salimma", from, to).await?;

let session = Session::kerberos(DEFAULT_BASE_URL)?; // blocking
session
    .add(&NewEntry::full_days(VACATION, "salimma", first, last))
    .await?;
```

## Installation

```sh
cargo add sandogasa-fedocal
```

## License

Licensed under either of

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT License](LICENSE-MIT)

at your option.
