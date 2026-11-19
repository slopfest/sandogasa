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
  for an existing entry. fedocal replaces every field on saving, so
  `edit` reads the edit form as a browser would send it and changes
  only the title, days or information asked for, keeping co-managers,
  reminders and the rest.

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
