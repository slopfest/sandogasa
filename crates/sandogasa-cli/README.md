# sandogasa-cli

Shared CLI utilities for sandogasa tools.

Provides helpers for common CLI patterns such as checking external tool
availability at startup and asking the user a yes/no question
(`confirm(question, default_yes)`, which prompts on stderr so stdout
stays clean for piped or `--json` output).

The optional `http` feature adds an `http` module with the plumbing
sandogasa's API clients share: `builder`/`blocking_builder` return a
reqwest client builder with the crypto provider installed and the
standard user agent and timeout set, and `ok`/`json_ok` (plus their
`blocking_` variants) turn a non-success response into a uniform
error naming the request, status, and body. It is off by default so
tools that do no networking don't pull in reqwest.

The optional `man` feature adds a `man` module that renders a clap
`Command` as a single roff man page, with each subcommand as a `.SS`
subsection so one page documents a whole CLI. `check::<Cli>(path)`
regenerates the page at `path` when `SANDOGASA_UPDATE_MAN` is set and
otherwise asserts that the committed page still documents every
visible flag and subcommand — the tools call it from a test, since a
binary's clap type is private to it. The feature is off by default and
enabled as a dev-dependency, so no binary carries the renderer.

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
