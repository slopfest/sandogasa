# sandogasa-discourse

A Rust client for the [Discourse](https://www.discourse.org/) forum API,
focused on user profile data.

## Features

- Fetch user profiles by username
- Access timezone, location, and last-posted timestamp
- Read custom user status (emoji, description, expiry)
- Optional API key authentication

## Usage

```rust
use sandogasa_discourse::DiscourseClient;

let client = DiscourseClient::new("https://discussion.fedoraproject.org");
let user = client.user("mattdm").await?;

println!("Timezone: {:?}", user.timezone);
println!("Location: {:?}", user.location);
println!("Last post: {:?}", user.last_posted_at);

if let Some(status) = &user.user_status {
    println!("Status: {:?} {:?}", status.emoji, status.description);
}
```

## License

Licensed under either of

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT License](LICENSE-MIT)

at your option.
