// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `srpm`, `mockbuild` and `verrel`: the checked-out SIG branch built
//! or read locally, with the macros CBS would build it with.
//!
//! CBS sets a collection's dist tag on its buildroot tag, as
//! `rpm.macro.*` values in the tag's extra settings: Hyperscale's Stream
//! collections set `dist` itself (`.hs.el10`), while Proposed Updates
//! and Extras set only a `distprefix` (`~proposed`) or `distsuffix`
//! (`s`, `_z`) for redhat-rpm-config to wrap around `.el<N>`. So the
//! macros are read from CBS (`koji -p cbs call getBuildConfig`) rather
//! than guessed, and passed as `--define`s to `rpmspec`, `rpmbuild` and
//! `mock`. On the host `dist` is spelled out, as the host's own
//! redhat-rpm-config is Fedora's; inside mock, the chroot's
//! redhat-rpm-config does it, as in CBS. centpkg-sig has `srpm`,
//! `mockbuild` and `verrel` too, but takes the dist tag from the
//! branch name alone, so a release package built in Extras
//! (`.el9s` in CBS) comes out as `.el9`.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

use sandogasa_cbs::Collection;
use sandogasa_cbs::lookaside::{Source, parse_sources, url};
use sandogasa_cli::explain::Ui;

use crate::add_pkg::{Checkout, VariantArgs, resolve};

/// `srpm`'s options: six, so one alphabetical list.
#[derive(clap::Args)]
pub struct SrpmArgs {
    /// The branch to build (default: the one checked out; another must
    /// be checked out first)
    #[arg(long, value_name = "BRANCH")]
    pub branch: Option<String>,

    /// Show each step and its command without running anything
    #[arg(long)]
    pub dry_run: bool,

    /// Narrate each step and show its command, pausing before each
    #[arg(long)]
    pub explain: bool,

    /// The package's dist-git checkout (default: the current
    /// directory)
    #[arg(short = 'C', long, value_name = "DIR")]
    pub repo: Option<PathBuf>,

    #[command(flatten)]
    pub variants: VariantArgs,
}

#[derive(clap::Args)]
pub struct MockbuildArgs {
    /// The branch to build (default: the one checked out; another must
    /// be checked out first)
    #[arg(long, value_name = "BRANCH", help_heading = "Selection")]
    pub branch: Option<String>,

    /// The package's dist-git checkout (default: the current
    /// directory)
    #[arg(short = 'C', long, value_name = "DIR", help_heading = "Selection")]
    pub repo: Option<PathBuf>,

    #[command(flatten, next_help_heading = "Selection")]
    pub variants: VariantArgs,

    /// The architecture to build for (default: this machine's)
    #[arg(long, value_name = "ARCH", help_heading = "Building")]
    pub arch: Option<String>,

    /// Show each step and its command without running anything
    #[arg(long, help_heading = "Writing")]
    pub dry_run: bool,

    /// Narrate each step and show its command, pausing before each
    #[arg(long, help_heading = "Output")]
    pub explain: bool,
}

/// `verrel`'s options: six, so one alphabetical list.
#[derive(clap::Args)]
pub struct VerrelArgs {
    /// The branch to read (default: the one checked out; another must
    /// be checked out first)
    #[arg(long, value_name = "BRANCH")]
    pub branch: Option<String>,

    /// Narrate each step and show its command (not with --json)
    #[arg(long)]
    pub explain: bool,

    /// Print the version-releases as JSON
    #[arg(long)]
    pub json: bool,

    /// The package's dist-git checkout (default: the current
    /// directory)
    #[arg(short = 'C', long, value_name = "DIR")]
    pub repo: Option<PathBuf>,

    #[command(flatten)]
    pub variants: VariantArgs,
}

/// Which checkout and branch a command works on.
struct Target<'a> {
    repo: PathBuf,
    branch: Option<&'a str>,
    variants: &'a VariantArgs,
}

/// An SRPM `srpm` built (or, in a dry run, would build): for which
/// collection, with the tag's macros, at which version-release.
#[derive(Debug)]
struct Built {
    collection: Collection,
    tag: Vec<(String, String)>,
    verrel: String,
    srpm: PathBuf,
}

/// One collection's version-release, for `verrel --json`.
#[derive(Debug, PartialEq, Eq, serde::Serialize)]
pub struct Verrel {
    pub target: String,
    pub verrel: String,
}

/// The checkout, its spec, and each collection with the tag's macros.
struct Prepared {
    repo: PathBuf,
    checkout: Checkout,
    spec: String,
    collections: Vec<(Collection, Vec<(String, String)>)>,
}

/// The command reading a buildroot tag's settings from CBS, as a person
/// would type it.
pub fn build_config_argv(tag: &str) -> Vec<String> {
    [
        "koji",
        "-p",
        "cbs",
        "call",
        "--json-output",
        "getBuildConfig",
        tag,
    ]
    .map(String::from)
    .to_vec()
}

/// The `rpm.macro.*` values in `getBuildConfig`'s answer, as
/// `(macro, value)`, sorted by macro.
pub fn tag_macros(config: &serde_json::Value) -> Vec<(String, String)> {
    let mut macros: Vec<(String, String)> = config["extra"]
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(k, v)| {
            let name = k.strip_prefix("rpm.macro.")?;
            let value = match v {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            Some((name.to_string(), value))
        })
        .collect();
    macros.sort();
    macros
}

/// The macros for building on the host: the tag's, with `dist` spelled
/// out where the tag gives only a prefix or suffix, as
/// redhat-rpm-config would on EL `major`. The host's own `%dist` is
/// Fedora's, so it cannot be left to expand.
pub fn host_macros(tag: &[(String, String)], major: u32) -> Vec<(String, String)> {
    let mut macros = tag.to_vec();
    if !macros.iter().any(|(k, _)| k == "dist") {
        let get = |name: &str| {
            tag.iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.as_str())
                .unwrap_or("")
        };
        let dist = format!("{}.el{major}{}", get("distprefix"), get("distsuffix"));
        macros.push(("dist".to_string(), dist));
        macros.sort();
    }
    macros
}

/// `macros` as `--define 'name value'` arguments.
pub fn define_args(macros: &[(String, String)]) -> Vec<String> {
    macros
        .iter()
        .flat_map(|(k, v)| ["--define".to_string(), format!("{k} {v}")])
        .collect()
}

/// The command printing the version-release `spec` builds with `macros`.
pub fn verrel_argv(spec: &str, macros: &[(String, String)]) -> Vec<String> {
    let mut argv: Vec<String> = [
        "rpmspec",
        "-q",
        "--srpm",
        "--qf",
        "%{version}-%{release}\\n",
    ]
    .map(String::from)
    .to_vec();
    argv.extend(define_args(macros));
    argv.push(spec.to_string());
    argv
}

/// The command building `spec`'s SRPM in `dir` with `macros`, its
/// sources read from `dir` and the SRPM written there, as `fedpkg srpm`
/// does.
pub fn srpm_argv(dir: &Path, spec: &str, macros: &[(String, String)]) -> Vec<String> {
    let dir = dir.display().to_string();
    let mut argv = vec!["rpmbuild".to_string(), "-bs".to_string()];
    for name in ["_sourcedir", "_specdir", "_srcrpmdir"] {
        argv.extend(["--define".to_string(), format!("{name} {dir}")]);
    }
    argv.extend(define_args(macros));
    argv.push(spec.to_string());
    argv
}

/// The SRPM `rpmbuild -bs` wrote, from its `Wrote: <path>` line.
pub fn wrote_srpm(output: &str) -> Option<PathBuf> {
    output
        .lines()
        .filter_map(|l| l.trim().strip_prefix("Wrote:"))
        .map(str::trim)
        .find(|p| p.ends_with(".src.rpm"))
        .map(PathBuf::from)
}

/// The command building `srpm` in mock's `config`, results in
/// `resultdir`, with `macros` (the tag's, for the chroot's
/// redhat-rpm-config to expand).
pub fn mock_argv(
    config: &str,
    resultdir: &Path,
    macros: &[(String, String)],
    srpm: &Path,
) -> Vec<String> {
    let mut argv = vec![
        "mock".to_string(),
        "-r".to_string(),
        config.to_string(),
        "--resultdir".to_string(),
        resultdir.display().to_string(),
    ];
    argv.extend(define_args(macros));
    argv.push(srpm.display().to_string());
    argv
}

/// Where `mockbuild` puts a build's results: `results_<package>/
/// <version>/<release>`, as `fedpkg mockbuild` does.
pub fn results_dir(repo: &Path, package: &str, verrel: &str) -> PathBuf {
    let (version, release) = verrel.rsplit_once('-').unwrap_or((verrel, ""));
    repo.join(format!("results_{package}"))
        .join(version)
        .join(release)
}

/// mock's name for the architecture Rust calls `arch`: the same, but
/// for 64-bit little-endian POWER, which Rust calls `powerpc64`.
pub fn mock_arch(arch: &str, little_endian: bool) -> String {
    match (arch, little_endian) {
        ("powerpc64", true) => "ppc64le".to_string(),
        (other, _) => other.to_string(),
    }
}

pub fn run_srpm(args: &SrpmArgs) -> ExitCode {
    let ui = Ui {
        explain: args.explain,
        dry_run: args.dry_run,
        quiet: false,
    };
    let target = Target {
        repo: args.repo.clone().unwrap_or_else(|| PathBuf::from(".")),
        branch: args.branch.as_deref(),
        variants: &args.variants,
    };
    exit(
        prepare(&ui, &target)
            .and_then(|p| srpm(&ui, &p))
            .map(|_| ()),
    )
}

pub fn run_mockbuild(args: &MockbuildArgs) -> ExitCode {
    exit(mockbuild(args))
}

pub fn run_verrel(args: &VerrelArgs) -> ExitCode {
    exit(verrel(args))
}

fn exit(result: Result<(), Box<dyn std::error::Error>>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Read each tag's settings from CBS, all at once: one `koji` each,
/// which costs a second of startup apiece, so they run side by side.
fn build_configs(tags: &[String]) -> Result<Vec<serde_json::Value>, Box<dyn std::error::Error>> {
    let children = tags
        .iter()
        .map(|tag| {
            let argv = build_config_argv(tag);
            Command::new(&argv[0])
                .args(&argv[1..])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .map_err(|e| format!("cannot run koji: {e}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut configs = Vec::new();
    for (tag, child) in tags.iter().zip(children) {
        let out = child.wait_with_output()?;
        if !out.status.success() {
            return Err(format!(
                "CBS has no build config for {tag}: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )
            .into());
        }
        configs.push(
            serde_json::from_slice(&out.stdout)
                .map_err(|e| format!("koji's answer for {tag} is not JSON: {e}"))?,
        );
    }
    Ok(configs)
}

/// Resolve the checkout and read each collection's macros from CBS.
/// Reading changes nothing, so it runs in a dry run too. The spec and
/// sources are the working tree's, so a `--branch` other than the one
/// checked out is refused rather than read with another branch's spec.
fn prepare(ui: &Ui, target: &Target<'_>) -> Result<Prepared, Box<dyn std::error::Error>> {
    let repo = target.repo.clone();
    let checkout = resolve(&repo, target.branch, None, Some(target.variants))?;
    if let Some(branch) = target.branch {
        let head = Command::new("git")
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .current_dir(&repo)
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default();
        if head != branch {
            return Err(format!(
                "{branch} is not checked out ({head} is): the spec and sources are read \
                 from the working tree, so check it out first (git switch {branch})"
            )
            .into());
        }
    }
    let spec = format!("{}.spec", checkout.package);
    if !repo.join(&spec).is_file() {
        return Err(format!("{} has no {spec}", repo.display()).into());
    }
    let tags: Vec<String> = checkout.collections.iter().map(|c| c.build_tag()).collect();
    ui.step(&format!(
        "Read the macros CBS builds with, from {}",
        tags.join(" and ")
    ));
    for tag in &tags {
        ui.show_command(&build_config_argv(tag));
    }
    if !ui.explain && !ui.dry_run {
        eprintln!("Reading CBS's build settings for {}…", tags.join(", "));
    }
    let configs = build_configs(&tags)?;
    let collections = checkout
        .collections
        .iter()
        .cloned()
        .zip(configs.iter().map(tag_macros))
        .collect();
    Ok(Prepared {
        repo,
        checkout,
        spec,
        collections,
    })
}

/// The version-release `spec` builds for each collection.
fn verrels(ui: &Ui, p: &Prepared) -> Result<Vec<Verrel>, Box<dyn std::error::Error>> {
    let mut out = Vec::new();
    for (collection, tag) in &p.collections {
        let argv = verrel_argv(&p.spec, &host_macros(tag, collection.major));
        ui.show_command(&argv);
        let run = Command::new(&argv[0])
            .args(&argv[1..])
            .current_dir(&p.repo)
            .output()
            .map_err(|e| format!("cannot run rpmspec: {e}"))?;
        let text = String::from_utf8_lossy(&run.stdout);
        let verrel = text.lines().next().map(str::trim).unwrap_or_default();
        if !run.status.success() || verrel.is_empty() {
            return Err(format!(
                "rpmspec could not read {}: {}",
                p.spec,
                String::from_utf8_lossy(&run.stderr).trim()
            )
            .into());
        }
        out.push(Verrel {
            target: collection.target(),
            verrel: verrel.to_string(),
        });
    }
    Ok(out)
}

fn verrel(args: &VerrelArgs) -> Result<(), Box<dyn std::error::Error>> {
    sandogasa_cli::require_tools(&[
        ("koji", "sudo dnf install koji", Some("--help")),
        ("rpmspec", "sudo dnf install rpm-build", Some("--version")),
    ])?;
    // Narration goes to stdout, which --json keeps for the answer.
    let ui = Ui {
        explain: args.explain && !args.json,
        dry_run: false,
        quiet: false,
    };
    let target = Target {
        repo: args.repo.clone().unwrap_or_else(|| PathBuf::from(".")),
        branch: args.branch.as_deref(),
        variants: &args.variants,
    };
    let p = prepare(&ui, &target)?;
    let found = verrels(&ui, &p)?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&found)?);
    } else if let [one] = found.as_slice() {
        println!("{}-{}", p.checkout.package, one.verrel);
    } else {
        for v in &found {
            println!("{}-{}  ({})", p.checkout.package, v.verrel, v.target);
        }
    }
    Ok(())
}

/// Fetch what the checkout's `sources` file names and the checkout
/// does not have, from the SIG lookaside at `base`, each checked
/// against its sum before it takes its name. A file that is there with
/// another sum is the packager's, perhaps regenerated: an error, never
/// overwritten. Nothing is fetched in a dry run.
fn fetch_sources(
    ui: &Ui,
    repo: &Path,
    package: &str,
    base: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let Ok(text) = std::fs::read_to_string(repo.join("sources")) else {
        return Ok(());
    };
    let sources = parse_sources(&text);
    if let Some(other) = sources.iter().find(|s| s.hash != "sha512") {
        return Err(format!(
            "{} is listed with {}; the SIG lookaside keeps sha512 sums",
            other.name, other.hash
        )
        .into());
    }
    let mut missing: Vec<&Source> = Vec::new();
    for source in &sources {
        if !repo.join(&source.name).exists() {
            missing.push(source);
        } else if !crate::upload_sources::matches(repo, source) {
            return Err(format!(
                "{} in the checkout does not match its sha512 sum in `sources`: update \
                 `sources` for it, or remove it to fetch the one `sources` names",
                source.name
            )
            .into());
        }
    }
    if missing.is_empty() {
        return Ok(());
    }
    ui.step(&format!(
        "Fetch the {} source file(s) the checkout lacks from the SIG lookaside",
        missing.len()
    ));
    let http = sandogasa_cli::http::blocking_builder(concat!(
        env!("CARGO_PKG_NAME"),
        "/",
        env!("CARGO_PKG_VERSION")
    ))
    .build()?;
    for source in missing {
        let at = url(base, package, source);
        ui.show_command(
            &[
                "curl",
                "--fail",
                "--location",
                "--output",
                &source.name,
                &at,
            ]
            .map(String::from),
        );
        if ui.dry_run {
            continue;
        }
        if !ui.explain {
            eprintln!("Fetching {} from the SIG lookaside…", source.name);
        }
        let mut resp = http
            .get(&at)
            .send()
            .map_err(|e| format!("cannot reach the SIG lookaside: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!(
                "the SIG lookaside answered HTTP {} for {}; upload it with \
                 `cbspkg upload-sources`, or put it in the checkout",
                resp.status().as_u16(),
                source.name
            )
            .into());
        }
        // Streamed to a file beside it, which takes the name only once
        // its sum is right: a tarball can be hundreds of megabytes.
        let partial = format!(".{}.part", source.name);
        let mut file = std::fs::File::create(repo.join(&partial))?;
        resp.copy_to(&mut file)?;
        drop(file);
        let candidate = Source {
            name: partial.clone(),
            ..source.clone()
        };
        if !crate::upload_sources::matches(repo, &candidate) {
            let _ = std::fs::remove_file(repo.join(&partial));
            return Err(format!(
                "{} from the SIG lookaside does not match its sha512 sum in `sources`",
                source.name
            )
            .into());
        }
        std::fs::rename(repo.join(&partial), repo.join(&source.name))?;
    }
    Ok(())
}

/// Build each collection's SRPM; a dry run says where rpmbuild would
/// write them.
fn srpm(ui: &Ui, p: &Prepared) -> Result<Vec<Built>, Box<dyn std::error::Error>> {
    sandogasa_cli::require_tools(&[
        ("rpmbuild", "sudo dnf install rpm-build", Some("--version")),
        ("rpmspec", "sudo dnf install rpm-build", Some("--version")),
        ("sha512sum", "sudo dnf install coreutils", Some("--version")),
    ])?;
    fetch_sources(
        ui,
        &p.repo,
        &p.checkout.package,
        &crate::upload_sources::lookaside_base(),
    )?;
    // Reading the version-release changes nothing: done in a dry run too.
    let verrels = verrels(
        &Ui {
            dry_run: false,
            ..*ui
        },
        p,
    )?;
    let dir = std::fs::canonicalize(&p.repo)?;
    let mut built = Vec::new();
    for ((collection, tag), v) in p.collections.iter().zip(verrels) {
        ui.step(&format!(
            "Build the SRPM for {}, as {}-{}",
            collection.target(),
            p.checkout.package,
            v.verrel
        ));
        let argv = srpm_argv(&dir, &p.spec, &host_macros(tag, collection.major));
        let (code, output) = ui.run_capture(&argv, &dir)?;
        let srpm = if ui.dry_run {
            // Where rpmbuild would write it, so what follows can say so.
            dir.join(format!("{}-{}.src.rpm", p.checkout.package, v.verrel))
        } else {
            if code != 0 {
                return Err(format!("rpmbuild failed:\n{}", output.trim()).into());
            }
            let path = wrote_srpm(&output)
                .ok_or_else(|| format!("rpmbuild named no SRPM:\n{}", output.trim()))?;
            println!("{}", path.display());
            path
        };
        built.push(Built {
            collection: collection.clone(),
            tag: tag.clone(),
            verrel: v.verrel,
            srpm,
        });
    }
    Ok(built)
}

fn mockbuild(args: &MockbuildArgs) -> Result<(), Box<dyn std::error::Error>> {
    sandogasa_cli::require_tools(&[
        ("koji", "sudo dnf install koji", Some("--help")),
        ("mock", "sudo dnf install mock", Some("--version")),
    ])?;
    let arch = args
        .arch
        .clone()
        .unwrap_or_else(|| mock_arch(std::env::consts::ARCH, cfg!(target_endian = "little")));
    let ui = Ui {
        explain: args.explain,
        dry_run: args.dry_run,
        quiet: false,
    };
    let target = Target {
        repo: args.repo.clone().unwrap_or_else(|| PathBuf::from(".")),
        branch: args.branch.as_deref(),
        variants: &args.variants,
    };
    let mut p = prepare(&ui, &target)?;
    // A collection mock has no config for is left to CBS: Facebook's
    // Stream branches also build for RHEL, which mock cannot set up.
    let (local, skipped): (Vec<_>, Vec<_>) = p
        .collections
        .drain(..)
        .partition(|(c, _)| c.mock_config(&arch).is_some());
    for (c, _) in &skipped {
        eprintln!(
            "Skipping {}: mock ships no config matching its buildroot; build it in CBS \
             with `cbspkg build --scratch`.",
            c.target()
        );
    }
    if local.is_empty() {
        return Err("mock can build none of the branch's collections".into());
    }
    p.collections = local;
    let dir = std::fs::canonicalize(&p.repo)?;
    for Built {
        collection,
        tag,
        verrel,
        srpm,
    } in srpm(&ui, &p)?
    {
        let config = collection.mock_config(&arch).expect("kept above");
        let resultdir = results_dir(&dir, &p.checkout.package, &verrel);
        ui.step(&format!(
            "Build it in mock's {config}, the buildroot {} matches",
            collection.target()
        ));
        if !ui.run(&mock_argv(&config, &resultdir, &tag, &srpm), &dir)? {
            return Err(format!("mock failed; its logs are in {}", resultdir.display()).into());
        }
        if !args.dry_run {
            println!("Results in {}", resultdir.display());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn macros(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    /// What CBS answered on 2026-11-11 for three collections' build tags.
    #[test]
    fn a_tags_macros_are_its_rpm_macro_settings() {
        let config = serde_json::json!({"extra": {
            "mock.package_manager": "dnf",
            "rpm.macro.vendor": "CentOS Proposed Updates SIG",
            "rpm.macro.distprefix": "~proposed",
            "mock.new_chroot": 1
        }});
        assert_eq!(
            tag_macros(&config),
            macros(&[
                ("distprefix", "~proposed"),
                ("vendor", "CentOS Proposed Updates SIG")
            ])
        );
        assert!(tag_macros(&serde_json::json!({})).is_empty());
    }

    #[test]
    fn dist_is_spelled_out_for_the_host_from_the_tags_parts() {
        // Hyperscale's Stream collection: dist itself.
        let hs = macros(&[("dist", ".hs.el10")]);
        assert_eq!(host_macros(&hs, 10), hs);
        // Proposed Updates: a prefix.
        assert_eq!(
            host_macros(&macros(&[("distprefix", "~proposed")]), 10),
            macros(&[("dist", "~proposed.el10"), ("distprefix", "~proposed")])
        );
        // Hyperscale's RHEL facebook build: both.
        assert_eq!(
            host_macros(
                &macros(&[("distprefix", ".hs+fb"), ("distsuffix", "_z")]),
                10
            )[0],
            ("dist".to_string(), ".hs+fb.el10_z".to_string())
        );
        // Extras' common collection: a suffix, which centpkg-sig misses.
        assert_eq!(
            host_macros(&macros(&[("distsuffix", "s")]), 9)[0],
            ("dist".to_string(), ".el9s".to_string())
        );
    }

    #[test]
    fn the_commands_are_what_a_person_would_type() {
        let m = macros(&[("dist", ".hs.el10")]);
        assert_eq!(
            sandogasa_cli::explain::command_line(&verrel_argv("perf.spec", &m)),
            "rpmspec -q --srpm --qf '%{version}-%{release}\\n' --define 'dist .hs.el10' perf.spec"
        );
        assert_eq!(
            sandogasa_cli::explain::command_line(&srpm_argv(Path::new("/w"), "perf.spec", &m)),
            "rpmbuild -bs --define '_sourcedir /w' --define '_specdir /w' \
             --define '_srcrpmdir /w' --define 'dist .hs.el10' perf.spec"
        );
        assert_eq!(
            sandogasa_cli::explain::command_line(&mock_argv(
                "centos-stream-hyperscale-10-x86_64",
                Path::new("/w/results_perf/6.1/1.hs.el10"),
                &m,
                Path::new("/w/perf-6.1-1.hs.el10.src.rpm"),
            )),
            "mock -r centos-stream-hyperscale-10-x86_64 --resultdir \
             /w/results_perf/6.1/1.hs.el10 --define 'dist .hs.el10' \
             /w/perf-6.1-1.hs.el10.src.rpm"
        );
        assert_eq!(
            sandogasa_cli::explain::command_line(&build_config_argv("t-build")),
            "koji -p cbs call --json-output getBuildConfig t-build"
        );
    }

    #[test]
    fn the_srpm_is_the_one_rpmbuild_wrote() {
        let out = "setting SOURCE_DATE_EPOCH=1\nWrote: /w/rsync-3.2.7-5~proposed.el10.src.rpm\n";
        assert_eq!(
            wrote_srpm(out),
            Some(PathBuf::from("/w/rsync-3.2.7-5~proposed.el10.src.rpm"))
        );
        assert_eq!(wrote_srpm("Wrote: /w/x.noarch.rpm\n"), None);
    }

    #[test]
    fn results_go_where_fedpkg_puts_them() {
        assert_eq!(
            results_dir(Path::new("/w"), "rsync", "3.2.7-5~proposed.el10"),
            PathBuf::from("/w/results_rsync/3.2.7/5~proposed.el10")
        );
    }

    mod run {
        use super::super::*;
        use crate::test_support::{EnvGuard, Fixture};

        const BRANCH: &str = "c10s-proposed-RHEL-1";
        const FILE: &str = "rsync-3.2.7.tar.gz";
        /// `printf 'data\n' | sha512sum`
        const SUM: &str = "73651d654c5ba73dd4b687f9dbbbdfc00884bf3dc1674e4cdcf762ff31778b29\
                           11e61e02b2a97c4055523eed2c4e6051b9902b0f91b4ca5b95e4c53cdf940b3d";

        /// A Proposed Updates checkout of rsync with fakes answering as
        /// CBS, rpmspec, rpmbuild and mock would.
        fn fixture(files: &[(&str, &str)]) -> Fixture {
            let f = Fixture::proposed(BRANCH, files);
            f.fake(
                "koji",
                &[
                    ("--help", ""),
                    (
                        "getBuildConfig proposed_updates10s-packages-main-el10s-build",
                        r#"{"extra": {"rpm.macro.distprefix": "~proposed", "mock.new_chroot": 1}}"#,
                    ),
                ],
            );
            f.fake(
                "rpmspec",
                &[
                    ("--version", ""),
                    ("dist ~proposed.el10", "3.2.7-5~proposed.el10"),
                ],
            );
            let srpm = f
                .repo
                .canonicalize()
                .unwrap()
                .join("rsync-3.2.7-5~proposed.el10.src.rpm");
            f.fake(
                "rpmbuild",
                &[
                    ("--version", ""),
                    ("-bs", &format!("Wrote: {}", srpm.display())),
                ],
            );
            f.fake("mock", &[("--version", ""), ("-r centos-stream-10-", "")]);
            f
        }

        fn target(f: &Fixture) -> Target<'static> {
            static NONE: std::sync::OnceLock<VariantArgs> = std::sync::OnceLock::new();
            Target {
                repo: f.repo.clone(),
                branch: None,
                variants: NONE.get_or_init(VariantArgs::default),
            }
        }

        fn ui(dry_run: bool) -> Ui {
            Ui {
                explain: false,
                dry_run,
                quiet: false,
            }
        }

        fn verrel_args(f: &Fixture, json: bool) -> VerrelArgs {
            VerrelArgs {
                branch: None,
                explain: true,
                json,
                repo: Some(f.repo.clone()),
                variants: VariantArgs::default(),
            }
        }

        fn mockbuild_args(f: &Fixture, arch: &str, dry_run: bool) -> MockbuildArgs {
            MockbuildArgs {
                branch: None,
                repo: Some(f.repo.clone()),
                variants: VariantArgs::default(),
                arch: Some(arch.to_string()),
                dry_run,
                explain: false,
            }
        }

        #[test]
        #[serial_test::serial]
        fn verrel_reads_the_dist_tag_from_cbs() {
            let f = fixture(&[]);
            let p = prepare(&ui(false), &target(&f)).unwrap();
            assert_eq!(
                verrels(&ui(false), &p).unwrap(),
                [Verrel {
                    target: "proposed_updates10s-packages-main-el10s".to_string(),
                    verrel: "3.2.7-5~proposed.el10".to_string(),
                }]
            );
            for json in [false, true] {
                assert_eq!(run_verrel(&verrel_args(&f, json)), ExitCode::SUCCESS);
            }
        }

        #[test]
        #[serial_test::serial]
        fn a_tag_cbs_does_not_know_is_an_error() {
            let f = fixture(&[]);
            f.fake(
                "koji",
                &[("--help", ""), ("getBuildConfig", "!No such tagInfo")],
            );
            let err = verrel(&verrel_args(&f, false)).unwrap_err().to_string();
            assert!(err.contains("CBS has no build config"), "{err}");
            assert!(err.contains("No such tagInfo"), "{err}");
        }

        #[test]
        #[serial_test::serial]
        fn a_checkout_without_its_spec_is_an_error() {
            let f = fixture(&[]);
            std::fs::remove_file(f.repo.join("rsync.spec")).unwrap();
            let err = verrel(&verrel_args(&f, false)).unwrap_err().to_string();
            assert!(err.ends_with("has no rsync.spec"), "{err}");
        }

        /// The spec is the working tree's, so another branch is refused
        /// rather than read with this one's spec.
        #[test]
        #[serial_test::serial]
        fn a_branch_not_checked_out_is_refused() {
            let f = fixture(&[]);
            f.git(&["branch", "c10s-proposed-RHEL-2"]);
            let mut args = verrel_args(&f, false);
            args.branch = Some("c10s-proposed-RHEL-2".to_string());
            let err = verrel(&args).unwrap_err().to_string();
            assert!(err.contains("is not checked out"), "{err}");
            args.branch = Some(BRANCH.to_string());
            verrel(&args).unwrap();
        }

        #[test]
        #[serial_test::serial]
        fn the_srpm_is_built_and_named() {
            let f = fixture(&[]);
            let p = prepare(&ui(false), &target(&f)).unwrap();
            let built = srpm(&ui(false), &p).unwrap();
            assert_eq!(built.len(), 1);
            let b = &built[0];
            assert_eq!(
                b.collection.target(),
                "proposed_updates10s-packages-main-el10s"
            );
            assert_eq!(b.tag, super::macros(&[("distprefix", "~proposed")]));
            assert_eq!(b.verrel, "3.2.7-5~proposed.el10");
            assert!(b.srpm.ends_with("rsync-3.2.7-5~proposed.el10.src.rpm"));
            // A dry run builds nothing but says where it would.
            f.fake("rpmbuild", &[("--version", "")]);
            let dry = srpm(&ui(true), &p).unwrap();
            assert!(dry[0].srpm.ends_with("rsync-3.2.7-5~proposed.el10.src.rpm"));
            let args = SrpmArgs {
                branch: None,
                dry_run: true,
                explain: false,
                repo: Some(f.repo.clone()),
                variants: VariantArgs::default(),
            };
            assert_eq!(run_srpm(&args), ExitCode::SUCCESS);
            // A failing rpmbuild is an error with its output.
            f.fake(
                "rpmbuild",
                &[("--version", ""), ("-bs", "!error: bad spec")],
            );
            let err = srpm(&ui(false), &p).unwrap_err().to_string();
            assert!(err.contains("bad spec"), "{err}");
        }

        #[test]
        #[serial_test::serial]
        fn mockbuild_builds_in_the_collections_config() {
            let f = fixture(&[]);
            mockbuild(&mockbuild_args(&f, "x86_64", false)).unwrap();
            assert_eq!(
                run_mockbuild(&mockbuild_args(&f, "x86_64", true)),
                ExitCode::SUCCESS
            );
            f.fake("mock", &[("--version", ""), ("-r", "!mock: build failed")]);
            let err = mockbuild(&mockbuild_args(&f, "x86_64", false))
                .unwrap_err()
                .to_string();
            assert!(err.starts_with("mock failed"), "{err}");
        }

        /// Facebook's Stream branch also builds for RHEL, which mock has
        /// no config for: that collection is skipped, the Stream one
        /// built.
        #[test]
        #[serial_test::serial]
        fn a_collection_mock_cannot_build_is_skipped() {
            let f = Fixture::hyperscale("perf", "c10s-hs+fb");
            f.fake(
                "koji",
                &[
                    ("--help", ""),
                    (
                        "hyperscale10s-packages-facebook-el10s-build",
                        r#"{"extra": {"rpm.macro.dist": ".hs+fb.el10"}}"#,
                    ),
                    (
                        "hyperscale10-packages-facebook-el10-build",
                        r#"{"extra": {"rpm.macro.distprefix": ".hs+fb", "rpm.macro.distsuffix": "_z"}}"#,
                    ),
                ],
            );
            f.fake(
                "rpmspec",
                &[("--version", ""), ("--srpm", "6.1-1.hs+fb.el10")],
            );
            f.fake("rpmbuild", &[("--version", "")]);
            f.fake("mock", &[("--version", "")]);
            let p = prepare(&ui(false), &target(&f)).unwrap();
            assert_eq!(p.collections.len(), 2, "Stream and RHEL");
            mockbuild(&mockbuild_args(&f, "x86_64", true)).unwrap();
            // asahi has no mock config at all: nothing to build.
            let f = Fixture::hyperscale("mesa", "c10s-hs+asahi");
            f.fake(
                "koji",
                &[("--help", ""), ("getBuildConfig", r#"{"extra": {}}"#)],
            );
            f.fake("mock", &[("--version", "")]);
            let err = mockbuild(&mockbuild_args(&f, "aarch64", true))
                .unwrap_err()
                .to_string();
            assert!(err.contains("mock can build none"), "{err}");
        }

        #[test]
        fn mock_names_powerpc_its_own_way() {
            assert_eq!(mock_arch("powerpc64", true), "ppc64le");
            assert_eq!(mock_arch("powerpc64", false), "powerpc64");
            assert_eq!(mock_arch("aarch64", true), "aarch64");
        }

        /// A source the checkout lacks comes from the SIG lookaside and
        /// is checked against its sum; one that does not match is an
        /// error and is not left behind.
        #[test]
        #[serial_test::serial]
        fn missing_sources_are_fetched_and_checked() {
            let f = fixture(&[("sources", &format!("SHA512 ({FILE}) = {SUM}\n"))]);
            let mut server = mockito::Server::new();
            let path = format!("/sources/rsync/{FILE}/sha512/{SUM}/{FILE}");
            let good = server
                .mock("GET", path.as_str())
                .with_body("data\n")
                .expect(1)
                .create();
            let _env = EnvGuard::new(&[("CBSPKG_LOOKASIDE_BASE", &server.url())]);
            let ui = Ui {
                explain: false,
                dry_run: false,
                quiet: false,
            };
            fetch_sources(&ui, &f.repo, "rsync", &server.url()).unwrap();
            good.assert();
            assert_eq!(
                std::fs::read_to_string(f.repo.join(FILE)).unwrap(),
                "data\n"
            );
            // There now: not fetched again.
            fetch_sources(&ui, &f.repo, "rsync", &server.url()).unwrap();

            // One there with another sum is the packager's: kept, and an
            // error rather than overwritten.
            std::fs::write(f.repo.join(FILE), "regenerated\n").unwrap();
            let err = fetch_sources(&ui, &f.repo, "rsync", &server.url())
                .unwrap_err()
                .to_string();
            assert!(err.contains("in the checkout does not match"), "{err}");
            assert_eq!(
                std::fs::read_to_string(f.repo.join(FILE)).unwrap(),
                "regenerated\n"
            );

            std::fs::remove_file(f.repo.join(FILE)).unwrap();
            good.remove();
            server
                .mock("GET", path.as_str())
                .with_body("other\n")
                .create();
            let err = fetch_sources(&ui, &f.repo, "rsync", &server.url())
                .unwrap_err()
                .to_string();
            assert!(
                err.contains("from the SIG lookaside does not match"),
                "{err}"
            );
            assert!(!f.repo.join(FILE).exists());
            assert!(!f.repo.join(format!(".{FILE}.part")).exists());

            server.reset();
            server.mock("GET", path.as_str()).with_status(404).create();
            let err = fetch_sources(&ui, &f.repo, "rsync", &server.url())
                .unwrap_err()
                .to_string();
            assert!(err.contains("HTTP 404"), "{err}");
            // A dry run fetches nothing.
            let dry = Ui {
                dry_run: true,
                ..ui
            };
            fetch_sources(&dry, &f.repo, "rsync", &server.url()).unwrap();
            assert!(!f.repo.join(FILE).exists());
        }

        #[test]
        #[serial_test::serial]
        fn a_source_with_another_hash_is_refused() {
            let f = fixture(&[("sources", "MD5 (old.tar.gz) = abc\n")]);
            let ui = Ui {
                explain: false,
                dry_run: true,
                quiet: false,
            };
            let err = fetch_sources(&ui, &f.repo, "rsync", "http://127.0.0.1:9")
                .unwrap_err()
                .to_string();
            assert!(err.contains("listed with md5"), "{err}");
        }
    }
}
