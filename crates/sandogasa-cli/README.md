# sandogasa-cli

Shared CLI utilities for sandogasa tools.

Provides helpers for common CLI patterns such as checking external tool
availability at startup.

The `defaults` module implements the workspace-wide flag-defaults
pattern: `parse_with_defaults::<Cli>(tool)` parses the command line
like `Cli::parse()`, then applies a `[defaults]` table from the
tool's `~/.config/<tool>/config.toml` for flags not given
explicitly — with command-line-wins precedence, conflict-aware
skipping, a `--no-defaults` escape hatch, and hard errors on
typo'd keys. See the root `DEVELOPMENT.md` for the pattern and the
config format.

## License

Licensed under either of

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT License](LICENSE-MIT)

at your option.
