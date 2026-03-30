# sandogasa-repology

A Rust client for the [Repology](https://repology.org/) package version
tracking API.

## Features

- Fetch package versions across distributions for a given project
- Filter by repository, find newest version, find latest Fedora stable or
  CentOS Stream entry
- Status-aware sorting with RPM version comparison for tie-breaking

## Usage

```rust
use sandogasa_repology::Client;

let client = Client::new();
let packages = client.get_project("ethtool")?;
let newest = sandogasa_repology::find_newest(&packages);
```

## License

This project is licensed under the [Mozilla Public License 2.0](https://mozilla.org/MPL/2.0/).
