# sandogasa-meetbot

HTTP client for [meetbot.fedoraproject.org][meetbot]'s meeting search
endpoint.

[meetbot]: https://meetbot.fedoraproject.org/

Meetbot exposes a single lightweight JSON endpoint at `/fragedpt/`
(used by its "Search for conversations" UI). This crate wraps that
endpoint behind a typed blocking client.

## Usage

```rust
use sandogasa_meetbot::Meetbot;

# fn demo() -> Result<(), Box<dyn std::error::Error>> {
let client = Meetbot::new();
for meeting in client.search("centos-hyperscale-sig")? {
    println!("{} {}", meeting.datetime, meeting.summary_url);
}
# Ok(())
# }
```

`search` returns a `Vec<Meeting>` sorted by date ascending.
Each `Meeting` carries the channel, start datetime (naive local),
topic, and public `summary_url` / `logs_url` rewritten onto
`meetbot.fedoraproject.org` regardless of the raw API's host.

Scope is deliberately minimal — additional endpoints can be added
as callers need them.

## License

Licensed under either of

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT License](LICENSE-MIT)

at your option.
