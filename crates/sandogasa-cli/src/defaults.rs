// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Flag defaults from the tool's config file.
//!
//! Every sandogasa tool lets users pin flag defaults in a
//! `[defaults]` table of its config — `/etc/<tool>/config.toml`
//! overridden per key by `~/.config/<tool>/config.toml` (see the
//! root DEVELOPMENT.md for the pattern):
//!
//! ```toml
//! [defaults]          # tool-wide
//! explain = true
//!
//! [defaults.update]   # for one subcommand only
//! quiet = true
//! ```
//!
//! Keys are the flag's **long name** (as typed on the command
//! line, dashes included). A top-level key covers global and
//! top-level flags, and also applies to any invoked subcommand
//! that has a flag of that name (a subcommand without it just
//! ignores the default) — so one `explain = true` line covers
//! every dbranch subcommand with `--explain`. Values: `true`
//! turns a boolean flag on (`false` is a no-op — flags can't be
//! un-set, so it just means "no default"); strings and numbers
//! become `--key value`; arrays repeat the flag per element.
//!
//! Precedence and safety rules, in order:
//! - anything given on the command line (or via a flag's env var)
//!   wins — a config default never overrides it;
//! - a default is silently skipped when it *conflicts* (per
//!   clap's `conflicts_with`) with an explicitly-given flag, so
//!   e.g. `--quiet` on the command line suppresses a configured
//!   `explain = true` rather than erroring;
//! - `--no-defaults` (added to every tool by this module) skips
//!   the whole table for one run;
//! - unknown keys are hard errors — a typo'd flag name must not
//!   be silently ignored.

use std::ffi::OsString;

use clap::parser::ValueSource;
use clap::{Arg, ArgAction, ArgMatches, Command, Parser};

/// The injected escape-hatch flag's id and long name.
const NO_DEFAULTS: &str = "no-defaults";

/// Parse the command line like `T::parse()`, applying `[defaults]`
/// from the tool's config layers (`/etc/<tool>/config.toml`, then
/// `~/.config/<tool>/config.toml` overriding it per key) for flags
/// not given on the command line. Call with the tool's crate name:
/// `parse_with_defaults::<Cli>(env!("CARGO_PKG_NAME"))`.
pub fn parse_with_defaults<T: Parser>(tool: &str) -> T {
    let argv: Vec<OsString> = std::env::args_os().collect();
    let cmd = augment_command(T::command());

    let matches = match cmd.clone().try_get_matches_from(&argv) {
        Ok(m) => m,
        // Includes --help / --version, which exit 0 from here.
        Err(e) => e.exit(),
    };

    let final_matches = if matches.get_flag(NO_DEFAULTS) {
        matches
    } else {
        match load_defaults(tool) {
            Ok(None) => matches,
            Ok(Some((table, sources))) => {
                let extra = match plan_injections(&cmd, &matches, &table) {
                    Ok(extra) => extra,
                    Err(e) => fail(&sources, &e),
                };
                if extra.is_empty() {
                    matches
                } else {
                    let mut full = argv;
                    full.extend(extra);
                    match cmd.try_get_matches_from(full) {
                        Ok(m) => m,
                        Err(e) => fail(&sources, &e.to_string()),
                    }
                }
            }
            Err(e) => {
                eprintln!("error: {e}");
                std::process::exit(2);
            }
        }
    };

    match T::from_arg_matches(&final_matches) {
        Ok(t) => t,
        Err(e) => e.exit(),
    }
}

fn fail(sources: &str, msg: &str) -> ! {
    eprintln!(
        "error: applying [defaults] from {sources}: {}",
        msg.trim_end()
    );
    eprintln!("(pass --no-defaults to skip them for this run)");
    std::process::exit(2);
}

/// Add the `--no-defaults` escape hatch to a tool's command.
fn augment_command(cmd: Command) -> Command {
    cmd.arg(
        Arg::new(NO_DEFAULTS)
            .long(NO_DEFAULTS)
            .global(true)
            .action(ArgAction::SetTrue)
            .help("Ignore the config file's [defaults] table"),
    )
}

/// Load the `[defaults]` table from the tool's config layers
/// (`/etc/<tool>/config.toml` overridden per key by
/// `~/.config/<tool>/config.toml`). `Ok(None)` when there is no
/// config dir, no file, or no table. The second tuple field
/// describes the sources for error messages.
type DefaultsTable = (toml::Table, String);
fn load_defaults(tool: &str) -> Result<Option<DefaultsTable>, String> {
    let Some(cfg) = sandogasa_config::ConfigFile::try_for_tool(tool) else {
        return Ok(None);
    };
    let sources = cfg.describe_sources();
    let Some(table) = cfg.read_merged()? else {
        return Ok(None);
    };
    match table.get("defaults") {
        None => Ok(None),
        Some(toml::Value::Table(t)) => Ok(Some((t.clone(), sources))),
        Some(_) => Err(format!("{sources}: [defaults] must be a table")),
    }
}

/// Compute the extra argv tokens the `[defaults]` table asks for,
/// given what the first parse saw. Pure over (command, matches,
/// table) so it's unit-testable.
fn plan_injections(
    cmd: &Command,
    matches: &ArgMatches,
    defaults: &toml::Table,
) -> Result<Vec<OsString>, String> {
    let mut extra = Vec::new();

    // Non-table entries apply to the top-level command (and
    // global args) — or, for flags that live on subcommands
    // (e.g. dbranch's --explain, present on several), to the
    // invoked subcommand when it has a flag of that name.
    for (key, value) in defaults {
        if let toml::Value::Table(sub_table) = value {
            // A nested table is a subcommand's defaults; validate
            // the name eagerly so typos don't rot silently.
            let Some(sub_cmd) = cmd.find_subcommand(key) else {
                return Err(format!("[defaults.{key}]: no such subcommand"));
            };
            // Only the invoked subcommand's table applies.
            let Some((invoked, sub_matches)) = matches.subcommand() else {
                continue;
            };
            if invoked != key {
                continue;
            }
            for (sub_key, sub_value) in sub_table {
                plan_one(
                    sub_cmd,
                    sub_matches,
                    Some((cmd, matches)),
                    &format!("{key}."),
                    sub_key,
                    sub_value,
                    &mut extra,
                )?;
            }
        } else if find_arg(cmd, key).is_some() {
            plan_one(cmd, matches, None, "", key, value, &mut extra)?;
        } else if let Some((invoked, sub_matches)) = matches.subcommand().filter(|(name, _)| {
            cmd.find_subcommand(name)
                .and_then(|s| find_arg(s, key))
                .is_some()
        }) {
            let sub_cmd = cmd.find_subcommand(invoked).expect("filtered above");
            plan_one(sub_cmd, sub_matches, None, "", key, value, &mut extra)?;
        } else if !cmd.get_subcommands().any(|s| find_arg(s, key).is_some()) {
            // Neither a top-level flag nor any subcommand's:
            // a typo. (Known on *some* subcommand but not the
            // invoked one is a silent no-op instead.)
            return Err(format!("[defaults.{key}]: no such flag --{key}"));
        }
    }
    Ok(extra)
}

/// Plan the tokens for one `key = value` entry in scope `cmd` /
/// `matches`. `parent` is the enclosing command for subcommand
/// scopes, so entries can also name global args defined there.
fn plan_one(
    cmd: &Command,
    matches: &ArgMatches,
    parent: Option<(&Command, &ArgMatches)>,
    scope: &str,
    key: &str,
    value: &toml::Value,
    extra: &mut Vec<OsString>,
) -> Result<(), String> {
    // Resolve the arg by its long name, falling back to the
    // parent's global args for subcommand scopes.
    let found = find_arg(cmd, key).map(|a| (a, cmd, matches)).or_else(|| {
        parent.and_then(|(p_cmd, p_matches)| {
            find_arg(p_cmd, key)
                .filter(|a| a.is_global_set())
                .map(|a| (a, p_cmd, p_matches))
        })
    });
    let Some((arg, arg_cmd, arg_matches)) = found else {
        return Err(format!("[defaults.{scope}{key}]: no such flag --{key}"));
    };
    if arg.get_id().as_str() == NO_DEFAULTS {
        return Err(format!(
            "[defaults.{scope}{key}]: --{key} cannot be a default"
        ));
    }

    // The command line (or an explicit env var) always wins.
    if given(arg_matches, arg.get_id().as_str()) {
        return Ok(());
    }
    // Skip a default that conflicts with something explicitly
    // given, instead of letting the re-parse error out. clap only
    // reports conflicts *declared by* the queried arg, so check
    // both directions: ours, and every given arg's declarations.
    let conflicts_with_given = arg_cmd
        .get_arg_conflicts_with(arg)
        .iter()
        .any(|c| given(arg_matches, c.get_id().as_str()))
        || arg_cmd.get_arguments().any(|g| {
            given(arg_matches, g.get_id().as_str())
                && arg_cmd
                    .get_arg_conflicts_with(g)
                    .iter()
                    .any(|c| c.get_id() == arg.get_id())
        });
    if conflicts_with_given {
        return Ok(());
    }

    let long = format!("--{key}");
    let is_switch = matches!(
        arg.get_action(),
        ArgAction::SetTrue | ArgAction::SetFalse | ArgAction::Count
    );
    match value {
        toml::Value::Boolean(true) if is_switch => extra.push(long.into()),
        // `false` on a switch is "no default", not an un-set.
        toml::Value::Boolean(false) if is_switch => {}
        toml::Value::String(s) if !is_switch => {
            extra.push(long.into());
            extra.push(s.into());
        }
        toml::Value::Integer(n) if !is_switch => {
            extra.push(long.into());
            extra.push(n.to_string().into());
        }
        toml::Value::Float(n) if !is_switch => {
            extra.push(long.into());
            extra.push(n.to_string().into());
        }
        toml::Value::Array(items) if !is_switch => {
            for item in items {
                let s = match item {
                    toml::Value::String(s) => s.clone(),
                    toml::Value::Integer(n) => n.to_string(),
                    toml::Value::Float(n) => n.to_string(),
                    other => {
                        return Err(format!(
                            "[defaults.{scope}{key}]: unsupported array element {other}"
                        ));
                    }
                };
                extra.push(long.clone().into());
                extra.push(s.into());
            }
        }
        other => {
            let kind = if is_switch {
                "a boolean flag (use true)"
            } else {
                "a value flag (use a string, number, or array)"
            };
            return Err(format!(
                "[defaults.{scope}{key}]: --{key} is {kind}, got {other}"
            ));
        }
    }
    Ok(())
}

/// Find an argument by its long name.
fn find_arg<'c>(cmd: &'c Command, long: &str) -> Option<&'c Arg> {
    cmd.get_arguments().find(|a| a.get_long() == Some(long))
}

/// Whether the user explicitly supplied this arg (command line or
/// its env var) — as opposed to a clap default.
fn given(matches: &ArgMatches, id: &str) -> bool {
    matches!(
        matches.value_source(id),
        Some(ValueSource::CommandLine) | Some(ValueSource::EnvVariable)
    )
}

#[cfg(test)]
mod tests {
    use clap::{CommandFactory, FromArgMatches};

    use super::*;

    #[derive(Parser, Debug)]
    #[command(name = "demo")]
    struct DemoCli {
        /// Global verbosity.
        #[arg(short, long, global = true)]
        verbose: bool,

        #[command(subcommand)]
        command: DemoCommand,
    }

    #[derive(clap::Subcommand, Debug)]
    enum DemoCommand {
        Update {
            #[arg(long)]
            explain: bool,
            #[arg(short, long, conflicts_with = "explain")]
            quiet: bool,
            #[arg(long)]
            branch: Vec<String>,
            #[arg(long, default_value_t = 3)]
            retries: u32,
        },
        Show,
    }

    fn plan(argv: &[&str], defaults: &str) -> Result<Vec<String>, String> {
        let cmd = augment_command(DemoCli::command());
        let matches = cmd.clone().try_get_matches_from(argv).unwrap();
        let table: toml::Table = defaults.parse().unwrap();
        plan_injections(&cmd, &matches, &table)
            .map(|v| v.into_iter().map(|s| s.into_string().unwrap()).collect())
    }

    #[test]
    fn injects_bool_flag_for_invoked_subcommand() {
        let extra = plan(&["demo", "update"], "[update]\nexplain = true").unwrap();
        assert_eq!(extra, vec!["--explain"]);
    }

    #[test]
    fn other_subcommands_defaults_do_not_apply() {
        let extra = plan(&["demo", "show"], "[update]\nexplain = true").unwrap();
        assert!(extra.is_empty());
    }

    #[test]
    fn command_line_wins_over_default() {
        // Value flag: explicit CLI value suppresses the default.
        let extra = plan(
            &["demo", "update", "--retries", "5"],
            "[update]\nretries = 9",
        )
        .unwrap();
        assert!(extra.is_empty());
    }

    #[test]
    fn conflicting_explicit_flag_suppresses_default() {
        // --quiet conflicts with explain; a configured explain
        // default must yield, not error.
        let extra = plan(&["demo", "update", "--quiet"], "[update]\nexplain = true").unwrap();
        assert!(extra.is_empty());
    }

    #[test]
    fn global_flag_default_applies_from_top_table() {
        let extra = plan(&["demo", "update"], "verbose = true").unwrap();
        assert_eq!(extra, vec!["--verbose"]);
    }

    #[test]
    fn top_level_key_reaches_subcommand_flag() {
        // A flag that lives on subcommands (not global) can be
        // defaulted tool-wide from the top-level table; it applies
        // to any invoked subcommand that has it.
        let extra = plan(&["demo", "update"], "explain = true").unwrap();
        assert_eq!(extra, vec!["--explain"]);
        // ...and is a silent no-op for subcommands without it.
        let extra = plan(&["demo", "show"], "explain = true").unwrap();
        assert!(extra.is_empty());
        // CLI-wins and conflict rules still hold in that scope.
        let extra = plan(&["demo", "update", "--quiet"], "explain = true").unwrap();
        assert!(extra.is_empty());
    }

    #[test]
    fn top_level_typo_still_errors() {
        let err = plan(&["demo", "show"], "explian = true").unwrap_err();
        assert!(err.contains("no such flag --explian"), "{err}");
    }

    #[test]
    fn subcommand_table_can_set_global_flag() {
        let extra = plan(&["demo", "update"], "[update]\nverbose = true").unwrap();
        assert_eq!(extra, vec!["--verbose"]);
    }

    #[test]
    fn arrays_repeat_value_flags() {
        let extra = plan(
            &["demo", "update"],
            "[update]\nbranch = [\"epel9\", \"epel10\"]",
        )
        .unwrap();
        assert_eq!(extra, vec!["--branch", "epel9", "--branch", "epel10"]);
    }

    #[test]
    fn numbers_become_values() {
        let extra = plan(&["demo", "update"], "[update]\nretries = 9").unwrap();
        assert_eq!(extra, vec!["--retries", "9"]);
    }

    #[test]
    fn false_is_a_no_op_for_switches() {
        let extra = plan(&["demo", "update"], "[update]\nexplain = false").unwrap();
        assert!(extra.is_empty());
    }

    #[test]
    fn unknown_flag_is_an_error() {
        let err = plan(&["demo", "update"], "[update]\nexplian = true").unwrap_err();
        assert!(err.contains("no such flag --explian"), "{err}");
    }

    #[test]
    fn unknown_subcommand_table_is_an_error() {
        let err = plan(&["demo", "show"], "[updaet]\nexplain = true").unwrap_err();
        assert!(err.contains("no such subcommand"), "{err}");
    }

    #[test]
    fn wrong_value_shape_is_an_error() {
        let err = plan(&["demo", "update"], "[update]\nexplain = \"yes\"").unwrap_err();
        assert!(err.contains("boolean flag"), "{err}");
        let err = plan(&["demo", "update"], "[update]\nretries = true").unwrap_err();
        assert!(err.contains("value flag"), "{err}");
    }

    #[test]
    fn no_defaults_flag_cannot_be_defaulted() {
        let err = plan(&["demo", "update"], "no-defaults = true").unwrap_err();
        assert!(err.contains("cannot be a default"), "{err}");
    }

    #[test]
    fn end_to_end_reparse_applies_defaults() {
        // Simulate the full flow: plan against the first parse,
        // then re-parse with the extra tokens appended.
        let cmd = augment_command(DemoCli::command());
        let argv = vec!["demo", "update"];
        let matches = cmd.clone().try_get_matches_from(&argv).unwrap();
        let table: toml::Table = "[update]\nexplain = true\nretries = 9".parse().unwrap();
        let extra = plan_injections(&cmd, &matches, &table).unwrap();
        let full: Vec<OsString> = argv.iter().map(OsString::from).chain(extra).collect();
        let final_matches = cmd.clone().try_get_matches_from(full).unwrap();
        let cli = DemoCli::from_arg_matches(&final_matches).unwrap();
        match cli.command {
            DemoCommand::Update {
                explain, retries, ..
            } => {
                assert!(explain);
                assert_eq!(retries, 9);
            }
            other => panic!("unexpected {other:?}"),
        }
    }
}
