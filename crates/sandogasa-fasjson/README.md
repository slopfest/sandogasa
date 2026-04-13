# sandogasa-fasjson

A Rust client for [FASJSON](https://fasjson.fedoraproject.org/), the Fedora
Account System JSON API, with Kerberos ticket management helpers.

## Features

- Fetch user profiles (username, emails, human name) via FASJSON
- Kerberos ticket status checking, renewal, and acquisition
- Read Fedora UPN from `~/.fedora.upn`

## Authentication

FASJSON requires Kerberos (GSSAPI) authentication. This crate shells out to
`curl --negotiate` for HTTP requests, avoiding a build-time dependency on
system krb5 libraries.

## Usage

```rust
use sandogasa_fasjson::{FasjsonClient, kerberos};

// Check ticket status and acquire if needed
let upn = kerberos::read_fedora_upn();
match kerberos::ticket_status() {
    kerberos::TicketStatus::Valid => {}
    kerberos::TicketStatus::ExpiredRenewable => {
        kerberos::renew_ticket().unwrap();
    }
    kerberos::TicketStatus::None => {
        let principal = format!("{}@FEDORAPROJECT.ORG", upn.unwrap());
        kerberos::acquire_ticket(&principal).unwrap();
    }
}

let client = FasjsonClient::new();
let user = client.user("salimma").unwrap();
println!("Emails: {:?}", user.emails);
```

## License

Licensed under either of

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT License](LICENSE-MIT)

at your option.
