# sandogasa-bodhi

A Rust client for the [Bodhi](https://bodhi.fedoraproject.org/) API,
used to query Fedora and EPEL updates and releases.

## Features

- Fetch updates for a package on a given release, with status filtering
- Paginate through all matching updates automatically
- Query active Fedora and EPEL releases (filtering out Flatpak, Container, ELN, and EPEL-Next)

## Usage

```rust
use sandogasa_bodhi::BodhiClient;

let client = BodhiClient::new();
let updates = client
    .updates_for_package("freerdp", "F42", &["stable", "testing"])
    .await?;
let releases = client.active_releases().await?;
```

## License

This project is licensed under the [Mozilla Public License 2.0](https://mozilla.org/MPL/2.0/).
