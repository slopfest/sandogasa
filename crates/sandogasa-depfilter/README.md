# sandogasa-depfilter

RPM dependency filtering for cross-branch analysis.

Provides functions to classify RPM dependency strings as auto-generated
or otherwise ignorable when comparing packages across Fedora/EPEL
branches, plus a default list of source packages to exclude from
installability checks.

## Installation

```sh
cargo install sandogasa-depfilter
```

## License

MPL-2.0
