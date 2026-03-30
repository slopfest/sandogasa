# sandogasa-fedrq

A Rust wrapper around the [fedrq](https://github.com/gotmax23/fedrq) CLI tool
for querying Fedora and EPEL RPM repositories.

## Features

- Query subpackage provides, requires, and names for a source RPM
- Find reverse dependencies (whatrequires) for a set of packages
- Query BuildRequires for source packages

## Usage

```rust
use sandogasa_fedrq::Fedrq;

let fq = Fedrq { branch: Some("rawhide".to_string()), ..Default::default() };
let provides = fq.subpkgs_provides("systemd")?;
```

## License

This project is licensed under the [Mozilla Public License 2.0](https://mozilla.org/MPL/2.0/).
