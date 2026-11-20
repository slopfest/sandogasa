// SPDX-License-Identifier: Apache-2.0 OR MIT

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use sandogasa_cli::style::{self, BOLD, ColorChoice, DIM, GREEN, RED, YELLOW, paint};
use specport::autotools::{self, Source};
use specport::{meson_options, source};

/// Port an RPM spec from one build system to another when upstream
/// switches, starting with autotools to Meson. The mechanical part is
/// done; the old settings stay in a comment above the new ones, with a
/// FIXME line for each that has no clean counterpart, for the packager
/// to check before deleting the comment.
#[derive(Parser)]
#[command(
    about,
    max_term_width = 80,
    version = sandogasa_cli::version!(),
    before_help = sandogasa_cli::banner!()
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Convert a spec's %configure and make steps to Meson's, mapping
    /// its configure options to the new source's Meson options
    AutotoolsToMeson(AutotoolsToMesonArgs),
}

#[derive(clap::Args)]
struct AutotoolsToMesonArgs {
    /// The spec to convert
    spec: PathBuf,

    /// Color the diff: auto (on a terminal), always or never
    #[arg(long, value_name = "WHEN", default_value = "auto")]
    color: ColorChoice,

    /// Print the converted spec and the notes as JSON
    #[arg(long, conflicts_with = "write")]
    json: bool,

    /// The new source: its unpacked tree, or its tarball
    #[arg(long, value_name = "PATH")]
    source: PathBuf,

    /// Write the converted spec in place, keeping the original as
    /// <spec>.orig, rather than showing a diff
    #[arg(long)]
    write: bool,
}

fn main() -> ExitCode {
    sandogasa_cli::init();
    let cli = sandogasa_cli::parse_with_defaults::<Cli>(env!("CARGO_PKG_NAME"));
    let result = match cli.command {
        Command::AutotoolsToMeson(args) => autotools_to_meson(&args),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// The new source's Meson options and meson.build.
fn read_source(path: &Path) -> Result<Source, String> {
    if !path.is_dir() {
        sandogasa_cli::require_tools(&[("tar", "sudo dnf install tar", Some("--version"))])
            .map_err(|e| e.to_string())?;
    }
    let [meson_build, options] = source::read_top(
        path,
        &[&["meson.build"], &["meson.options", "meson_options.txt"]],
    )?
    .try_into()
    .expect("one answer per list");
    let meson_build =
        meson_build.ok_or_else(|| format!("{} has no meson.build at its top", path.display()))?;
    let options = match options {
        Some(text) => meson_options::parse(&text).map_err(|e| format!("Meson options: {e}"))?,
        None => Vec::new(),
    };
    let (meson_files, top) = source::read_tree(path)?;
    Ok(Source {
        options,
        meson_build,
        meson_files,
        top,
    })
}

/// `diff`, a unified diff of a spec, colored for reading: lines taken
/// out red and lines put in green, as `git diff` shows them, but an
/// added FIXME line yellow, to look at, and the rest of an added
/// specport comment gray; file headers bold, hunk headers gray.
fn paint_diff(diff: &str, color: bool) -> String {
    if !color {
        return diff.to_string();
    }
    diff.lines()
        .map(|l| {
            let sgr = if l.starts_with("---") || l.starts_with("+++") {
                BOLD
            } else if l.starts_with("@@") {
                DIM
            } else if let Some(added) = l.strip_prefix('+') {
                let t = added.trim_start();
                if t.starts_with("# FIXME specport") {
                    YELLOW
                } else if t.starts_with("# specport") || t.starts_with("#   ") {
                    DIM
                } else {
                    GREEN
                }
            } else if l.starts_with('-') {
                RED
            } else {
                return format!("{l}\n");
            };
            format!("{}\n", paint(l, sgr, true))
        })
        .collect()
}

/// A note for standard error: a FIXME yellow, to look at, the rest
/// gray.
fn paint_note(note: &str, color: bool) -> String {
    let sgr = if note.starts_with("FIXME") {
        YELLOW
    } else {
        DIM
    };
    paint(note, sgr, color)
}

/// What to do next, for standard error: without `--write`, the command
/// that writes the spec (bold green, a command to run); with it, the
/// FIXME lines to check, the comments to delete, and the build.
fn next_steps(args: &AutotoolsToMesonArgs, fixmes: usize, color: bool) -> String {
    if !args.write {
        let argv = [
            "specport".to_string(),
            "autotools-to-meson".to_string(),
            "--source".to_string(),
            args.source.display().to_string(),
            "--write".to_string(),
            args.spec.display().to_string(),
        ];
        let command = sandogasa_cli::explain::command_line(&argv);
        return format!(
            "\nNothing written. If this looks right, write it with:\n  {}",
            paint(&command, &format!("{BOLD}{GREEN}"), color)
        );
    }
    let check = match fixmes {
        0 => String::new(),
        1 => "check the `# FIXME specport` line, then ".to_string(),
        n => format!("check the {n} `# FIXME specport` lines, then "),
    };
    format!(
        "\nNext: {check}delete the `# specport` comments once settled, and build it: \
         a Meson build may install to other paths than %files lists."
    )
}

/// Save `text`, the spec at `spec` before conversion, as `<spec>.orig`
/// (as `patch -b` names it), or `.orig.1`, `.orig.2`, … when that is
/// taken: a backup is never overwritten, so a second run cannot replace
/// the original with an already converted spec. Returns the path.
fn back_up(spec: &Path, text: &str) -> Result<PathBuf, String> {
    use std::fs::OpenOptions;
    let base = format!("{}.orig", spec.display());
    for n in 0.. {
        let path = PathBuf::from(if n == 0 {
            base.clone()
        } else {
            format!("{base}.{n}")
        });
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut f) => {
                f.write_all(text.as_bytes())
                    .map_err(|e| format!("{}: {e}", path.display()))?;
                return Ok(path);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("{}: {e}", path.display())),
        }
    }
    unreachable!("some backup name is free")
}

/// `diff -u` of `before` (the file at `path`) and `after`.
fn diff(path: &Path, after: &str) -> Result<String, String> {
    let mut tmp = tempfile::NamedTempFile::new().map_err(|e| e.to_string())?;
    tmp.write_all(after.as_bytes()).map_err(|e| e.to_string())?;
    let label = path.display().to_string();
    let out = std::process::Command::new("diff")
        .args(["-u", "--label", &label, "--label", &label])
        .arg(path)
        .arg(tmp.path())
        .output()
        .map_err(|e| format!("cannot run diff: {e}"))?;
    // 0: the same, 1: they differ, 2: trouble.
    if out.status.code() == Some(2) {
        return Err(format!(
            "diff: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn autotools_to_meson(args: &AutotoolsToMesonArgs) -> Result<(), String> {
    if !args.write && !args.json {
        sandogasa_cli::require_tools(&[("diff", "sudo dnf install diffutils", Some("--version"))])
            .map_err(|e| e.to_string())?;
    }
    let spec =
        std::fs::read_to_string(&args.spec).map_err(|e| format!("{}: {e}", args.spec.display()))?;
    let source = read_source(&args.source)?;
    if source.options.is_empty() {
        eprintln!(
            "note: {} declares no Meson options; every configure option needs a FIXME",
            args.source.display()
        );
    }
    let converted = autotools::convert(&spec, &source);
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&converted).map_err(|e| e.to_string())?
        );
        return Ok(());
    }
    if args.write {
        let unchanged = converted
            .notes
            .iter()
            .any(|n| n.contains("nothing changed"));
        if unchanged {
            eprintln!("Nothing to write to {}.", args.spec.display());
        } else {
            let kept = back_up(&args.spec, &spec)?;
            std::fs::write(&args.spec, &converted.spec)
                .map_err(|e| format!("{}: {e}", args.spec.display()))?;
            eprintln!(
                "Wrote {}; the original is in {}.",
                args.spec.display(),
                kept.display()
            );
        }
    } else {
        let color = style::use_color(args.color);
        print!("{}", paint_diff(&diff(&args.spec, &converted.spec)?, color));
    }
    let color = style::stderr_color();
    for note in &converted.notes {
        eprintln!("{}", paint_note(note, color));
    }
    let fixmes = converted
        .spec
        .lines()
        .filter(|l| l.trim_start().starts_with("# FIXME specport"))
        .count();
    if !converted
        .notes
        .iter()
        .any(|n| n.contains("nothing changed"))
    {
        eprintln!("{}", next_steps(args, fixmes, color));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The committed man page is generated from this CLI; see
    /// `sandogasa_cli::man` and `scripts/gen-man.sh`.
    #[test]
    fn man_page_matches_cli() {
        sandogasa_cli::man::check::<super::Cli>(
            concat!(env!("CARGO_MANIFEST_DIR"), "/man/specport.1"),
            env!("CARGO_PKG_VERSION"),
        );
    }

    fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("pkg-1.0");
        std::fs::create_dir(&src).unwrap();
        std::fs::write(src.join("meson.build"), "project('pkg')\n").unwrap();
        std::fs::write(
            src.join("meson_options.txt"),
            "option('aer', type : 'feature')\n",
        )
        .unwrap();
        let spec = dir.path().join("pkg.spec");
        std::fs::write(
            &spec,
            "BuildRequires: gcc\n%build\n%configure --enable-aer\nmake\n",
        )
        .unwrap();
        (dir, spec, src)
    }

    #[test]
    fn a_spec_is_written_in_place_or_shown_as_a_diff() {
        let (_dir, spec, src) = fixture();
        let mut args = AutotoolsToMesonArgs {
            color: ColorChoice::Never,
            spec: spec.clone(),
            json: false,
            source: src.clone(),
            write: false,
        };
        let before = std::fs::read_to_string(&spec).unwrap();
        let shown = diff(&spec, "changed\n").unwrap();
        assert!(shown.contains("+changed"), "{shown}");
        autotools_to_meson(&args).unwrap();
        assert_eq!(std::fs::read_to_string(&spec).unwrap(), before);
        args.json = true;
        autotools_to_meson(&args).unwrap();
        args.json = false;
        args.write = true;
        autotools_to_meson(&args).unwrap();
        let after = std::fs::read_to_string(&spec).unwrap();
        let mut orig = spec.clone().into_os_string();
        orig.push(".orig");
        assert_eq!(std::fs::read_to_string(orig).unwrap(), before);
        assert!(
            after.contains("%meson -Daer=enabled\n%meson_build\n"),
            "{after}"
        );
        assert!(after.contains("BuildRequires: meson\n"), "{after}");
    }

    #[test]
    fn a_source_without_meson_build_is_refused() {
        let (_dir, spec, src) = fixture();
        std::fs::remove_file(src.join("meson.build")).unwrap();
        let args = AutotoolsToMesonArgs {
            color: ColorChoice::Never,
            spec,
            json: true,
            source: src,
            write: false,
        };
        let err = autotools_to_meson(&args).unwrap_err();
        assert!(err.contains("no meson.build"), "{err}");
    }

    #[test]
    fn a_diff_is_colored_by_what_each_line_is() {
        let diff = "--- a\n+++ a\n@@ -1 +1 @@\n-old\n+new\n+# FIXME specport: x\n\
                    +# specport: y\n+#   %%configure\n same\n";
        assert_eq!(paint_diff(diff, false), diff);
        let painted = paint_diff(diff, true);
        let lines: Vec<&str> = painted.lines().collect();
        assert_eq!(lines[0], format!("{BOLD}--- a{}", style::RESET));
        assert!(lines[2].starts_with(DIM));
        assert!(lines[3].starts_with(RED));
        assert!(lines[4].starts_with(GREEN));
        assert!(lines[5].starts_with(YELLOW));
        assert!(lines[6].starts_with(DIM) && lines[7].starts_with(DIM));
        assert_eq!(lines[8], " same");
        assert!(paint_note("FIXME: x", true).starts_with(YELLOW));
        assert!(paint_note("dropped x", true).starts_with(DIM));
        assert_eq!(paint_note("dropped x", false), "dropped x");
    }

    #[test]
    fn the_next_step_is_the_write_or_what_to_check() {
        let mut args = AutotoolsToMesonArgs {
            color: ColorChoice::Never,
            spec: PathBuf::from("my pkg.spec"),
            json: false,
            source: PathBuf::from("pkg-1.0.tar.xz"),
            write: false,
        };
        let shown = next_steps(&args, 2, false);
        assert!(
            shown.ends_with(
                "specport autotools-to-meson --source pkg-1.0.tar.xz --write 'my pkg.spec'"
            ),
            "{shown}"
        );
        assert!(next_steps(&args, 2, true).contains(&format!("{BOLD}{GREEN}specport")));
        args.write = true;
        assert!(next_steps(&args, 2, false).contains("check the 2 `# FIXME specport` lines"));
        assert!(next_steps(&args, 1, false).contains("check the `# FIXME specport` line,"));
        assert!(next_steps(&args, 0, false).starts_with("\nNext: delete"));
    }

    #[test]
    fn a_backup_is_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let spec = dir.path().join("pkg.spec");
        let first = back_up(&spec, "one\n").unwrap();
        let second = back_up(&spec, "two\n").unwrap();
        assert_eq!(first, dir.path().join("pkg.spec.orig"));
        assert_eq!(second, dir.path().join("pkg.spec.orig.1"));
        assert_eq!(std::fs::read_to_string(first).unwrap(), "one\n");
        assert!(back_up(&dir.path().join("missing/pkg.spec"), "x").is_err());
    }
}
