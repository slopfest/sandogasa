# sandogasa-cli

Shared CLI utilities for sandogasa tools.

Provides helpers for common CLI patterns such as checking external tool
availability at startup and asking the user a yes/no question
(`confirm(question, default_yes)`, which prompts on stderr so stdout
stays clean for piped or `--json` output). `confirm_each` asks the
same way about a list the caller has already printed: all of it, none
of it, or `each` to walk it item by item, returning the indices taken.

The `retry` module is the one place a client decides what to retry: a
transport error, or a 429, 500, 502, 503 or 504. `send` and
`send_blocking` resend a reqwest request (with the `http` feature),
returning the last response when the attempts run out, so the caller
judges its status. `retry` and `retry_blocking` take a predicate for
anything else, such as a Koji XML-RPC call. A `Policy` sets the
attempts and the backoff (doubling from a base delay, honouring a
`Retry-After`, never over a cap); `Policy::immediate` waits for
nothing, for tests.

The `cache` module keeps answers between runs (`DiskCache`, one file
per key, each cache with its own lifetime) and lets a tool expire what
it keeps: `mark_used(entry)` records a use in a stamp beside the entry
(`.<name>.last-used`, never inside it), `last_used(entry)` reads it
back (an entry never marked counts from its modification time), and
`prune(dir, max_age, dry_run)` removes, whole, the entries directly
under `dir` unused for longer than `max_age`, never less than a day,
reporting what it removed and kept with their sizes.

The `progress` module draws a one-line counter on stderr for a phase
that works through a known number of items (`Progress::stderr(label,
total, verbose)`, then `tick(item)` as each starts, from any thread):
`reading new provides 23/39: evolution-data-server`, rewritten in
place, the count padded to the total's width so the colon stays in one
column, cut to 79 characters so it never wraps. It is drawn only when
stderr is a terminal and the run is not verbose, `eprintln` prints a
line without running into it, and dropping it clears the line.

The optional `explain` feature adds an `explain` module for a tool
whose steps are commands a person could run by hand: `Ui { explain,
dry_run, quiet }` narrates each step (`step`), shows its command
exactly as it would be typed (`show_command`), and runs it
(`run_required`, `run_capture`, …), pausing for Enter first under
`--explain` and running nothing under `--dry-run`. Colour goes through
`anstream`, which drops it for a pipe or with `NO_COLOR`. It came from
dbranch, whose `--explain` the workspace rule names as the model.

`ConfigArgs` holds the arguments every tool's `config` command takes
(`Config(sandogasa_cli::ConfigArgs)`), so the flags and their help read
the same everywhere. Today that is `--all`, which asks for every
setting again with the saved value as the default (see
`sandogasa_config::ask`).

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
