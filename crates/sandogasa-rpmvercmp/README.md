# sandogasa-rpmvercmp

A pure Rust implementation of the RPM version comparison algorithm (`rpmvercmp`)
and EVR (epoch:version-release) comparison.

## Features

- Compare version strings using the RPM vercmp algorithm
- Parse and compare full EVR (epoch:version-release) strings
- Zero dependencies beyond `std`

## Usage

```rust
use sandogasa_rpmvercmp::{rpmvercmp, compare_evr};
use std::cmp::Ordering;

assert_eq!(rpmvercmp("1.10", "1.9"), Ordering::Greater);
assert_eq!(compare_evr("2:1.0-1", "1:2.0-1"), Ordering::Greater);
```

## License

This project is licensed under the [Mozilla Public License 2.0](https://mozilla.org/MPL/2.0/).
