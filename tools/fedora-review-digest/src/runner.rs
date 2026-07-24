// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Invoke `fedora-review` itself (the `run` subcommand).
//!
//! The motivating case: staged mass updates live in a COPR before
//! anything lands in Rawhide, so a new package under review can
//! depend on versions Rawhide doesn't have yet (observed with
//! rust-gufo-svg, rhbz#2497354, needing rust-gufo-common ≥
//! 2.0.0~alpha while Rawhide had 1.1 — the fix was building with
//! the `decathorpe/glycin-next` COPR enabled). Extra repos reach
//! mock via `--addrepo`, threaded through `fedora-review -o`.

use std::path::Path;

/// `fedora-review`'s own default mock options (from `--help`,
/// fedora-review 0.11). Passing `-o` REPLACES these rather than
/// appending, so any options we inject must restate them.
pub const DEFAULT_MOCK_OPTIONS: &str =
    "--no-cleanup-after --no-clean --plugin-option=tmpfs:keep_mounted=True";

/// The COPR results host serving repo metadata per chroot.
const COPR_RESULTS: &str = "https://download.copr.fedorainfracloud.org/results";

/// Repo baseurl for a COPR given as `owner/project` (a leading
/// `@` marks a group, as in COPR itself), for one chroot (e.g.
/// `fedora-rawhide-aarch64` — mock config names double as COPR
/// chroot names).
pub fn copr_repo_url(spec: &str, chroot: &str) -> Result<String, String> {
    let (owner, project) = spec.split_once('/').ok_or_else(|| {
        format!("invalid COPR '{spec}': expected owner/project (e.g. decathorpe/glycin-next)")
    })?;
    let name_ok =
        |s: &str| !s.is_empty() && s.chars().all(|c| c.is_alphanumeric() || "-_.@".contains(c));
    if !name_ok(owner) || !name_ok(project) || project.contains('@') {
        return Err(format!(
            "invalid COPR '{spec}': expected owner/project (e.g. decathorpe/glycin-next)"
        ));
    }
    Ok(format!("{COPR_RESULTS}/{owner}/{project}/{chroot}/"))
}

/// The mock chroot / config name extra repos are resolved for:
/// the `--mock-config` value when given, else fedora-review's own
/// default of `fedora-rawhide-<host arch>`.
pub fn chroot_name(mock_config: Option<&str>) -> String {
    match mock_config {
        Some(cfg) => cfg.to_string(),
        None => format!("fedora-rawhide-{}", std::env::consts::ARCH),
    }
}

/// Everything that shapes the `fedora-review` invocation.
#[derive(Default)]
pub struct RunSpec {
    pub bug: String,
    /// COPRs as `owner/project`, resolved to repo URLs per chroot.
    pub coprs: Vec<String>,
    /// Raw repo baseurls, passed through as-is.
    pub repos: Vec<String>,
    pub mock_config: Option<String>,
    /// mock `--uniqueext` suffix, so the review's buildroot doesn't
    /// collide with a mock build already running in the same chroot
    /// (mock doesn't abort cleanly when the chroot is in use).
    pub uniqueext: Option<String>,
    /// Arbitrary extra mock options, appended verbatim.
    pub mock_options: Vec<String>,
}

/// Assemble the `fedora-review` argument vector.
pub fn build_args(spec: &RunSpec) -> Result<Vec<String>, String> {
    let bug = &spec.bug;
    if bug.is_empty() || !bug.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!("'{bug}' is not a Bugzilla bug id"));
    }
    let mut args = vec!["-b".to_string(), bug.to_string()];
    if let Some(cfg) = &spec.mock_config {
        args.push("-m".to_string());
        args.push(cfg.to_string());
    }
    let chroot = chroot_name(spec.mock_config.as_deref());
    // Everything destined for `fedora-review -o` (one space-separated
    // string that fedora-review re-splits into mock arguments — so no
    // entry may itself contain whitespace).
    let mut extra: Vec<String> = Vec::new();
    for copr in &spec.coprs {
        extra.push(format!("--addrepo={}", copr_repo_url(copr, &chroot)?));
    }
    for url in &spec.repos {
        extra.push(format!("--addrepo={url}"));
    }
    if let Some(ext) = &spec.uniqueext {
        if ext.is_empty() || ext.chars().any(|c| c.is_whitespace()) {
            return Err(format!(
                "invalid --uniqueext '{ext}': must be a non-empty suffix without whitespace"
            ));
        }
        extra.push(format!("--uniqueext={ext}"));
    }
    for opt in &spec.mock_options {
        if opt.is_empty() || opt.chars().any(|c| c.is_whitespace()) {
            return Err(format!(
                "invalid mock option '{opt}': fedora-review -o splits on whitespace, so each option must be a single token (use --opt=value form)"
            ));
        }
        extra.push(opt.clone());
    }
    if !extra.is_empty() {
        let mut mock_options = DEFAULT_MOCK_OPTIONS.to_string();
        for entry in &extra {
            mock_options.push(' ');
            mock_options.push_str(entry);
        }
        args.push("-o".to_string());
        args.push(mock_options);
    }
    Ok(args)
}

/// Render the command for display (`--dry-run`, error messages).
pub fn display_command(args: &[String]) -> String {
    let mut out = String::from("fedora-review");
    for arg in args {
        if arg.contains(' ') {
            out.push_str(&format!(" '{arg}'"));
        } else {
            out.push_str(&format!(" {arg}"));
        }
    }
    out
}

/// Run `fedora-review` in `reviews_dir` with output streaming to
/// the terminal (mock builds run for minutes; the user should see
/// progress). The result directory lands as
/// `<reviews_dir>/<bug>-<name>/`.
pub fn run_fedora_review(args: &[String], reviews_dir: &Path) -> Result<(), String> {
    if !sandogasa_cli::tool_exists("fedora-review") {
        return Err("fedora-review not found on PATH.\n\
             Install it with: sudo dnf install fedora-review"
            .to_string());
    }
    eprintln!("running: {}", display_command(args));
    let status = std::process::Command::new("fedora-review")
        .args(args)
        .current_dir(reviews_dir)
        .status()
        .map_err(|e| format!("failed to run fedora-review: {e}"))?;
    if !status.success() {
        return Err(format!(
            "fedora-review exited with {status}; partial results (build \
             logs, root.log) may still be under {}",
            reviews_dir.display()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copr_url_for_user_and_group() {
        assert_eq!(
            copr_repo_url("decathorpe/glycin-next", "fedora-rawhide-x86_64").unwrap(),
            "https://download.copr.fedorainfracloud.org/results/\
             decathorpe/glycin-next/fedora-rawhide-x86_64/"
                .replace(" ", "")
        );
        assert!(
            copr_repo_url("@rust/staging", "fedora-rawhide-aarch64")
                .unwrap()
                .contains("/@rust/staging/")
        );
    }

    #[test]
    fn copr_spec_validation() {
        assert!(copr_repo_url("no-slash", "c").is_err());
        assert!(copr_repo_url("/project", "c").is_err());
        assert!(copr_repo_url("owner/", "c").is_err());
        assert!(copr_repo_url("owner/pro ject", "c").is_err());
        assert!(copr_repo_url("owner/pro/ject", "c").is_err());
    }

    #[test]
    fn chroot_follows_mock_config() {
        assert_eq!(chroot_name(Some("fedora-42-x86_64")), "fedora-42-x86_64");
        assert!(chroot_name(None).starts_with("fedora-rawhide-"));
    }

    fn spec(bug: &str) -> RunSpec {
        RunSpec {
            bug: bug.to_string(),
            ..RunSpec::default()
        }
    }

    #[test]
    fn build_args_plain_bug_has_no_mock_options() {
        let args = build_args(&spec("2497354")).unwrap();
        assert_eq!(args, vec!["-b", "2497354"]);
    }

    #[test]
    fn build_args_preserves_default_mock_options() {
        // fedora-review's -o REPLACES its defaults; ours must
        // restate them ahead of the addrepo entries.
        let args = build_args(&RunSpec {
            coprs: vec!["decathorpe/glycin-next".to_string()],
            mock_config: Some("fedora-rawhide-x86_64".to_string()),
            ..spec("2497354")
        })
        .unwrap();
        assert_eq!(args[0..4], ["-b", "2497354", "-m", "fedora-rawhide-x86_64"]);
        assert_eq!(args[4], "-o");
        assert!(args[5].starts_with(DEFAULT_MOCK_OPTIONS), "{}", args[5]);
        assert!(
            args[5].ends_with(
                "--addrepo=https://download.copr.fedorainfracloud.org/results/\
                 decathorpe/glycin-next/fedora-rawhide-x86_64/"
                    .replace(" ", "")
                    .as_str()
            ),
            "{}",
            args[5]
        );
    }

    #[test]
    fn build_args_mixes_coprs_and_raw_repos() {
        let args = build_args(&RunSpec {
            coprs: vec!["a/b".to_string()],
            repos: vec!["https://example.org/repo/".to_string()],
            ..spec("1")
        })
        .unwrap();
        let opts = &args[3];
        assert_eq!(opts.matches("--addrepo=").count(), 2);
        assert!(opts.contains("--addrepo=https://example.org/repo/"));
    }

    #[test]
    fn build_args_uniqueext_alone_still_emits_mock_options() {
        // A parallel-buildroot suffix without any extra repos must
        // still produce -o (with the defaults restated).
        let args = build_args(&RunSpec {
            uniqueext: Some("review".to_string()),
            ..spec("1")
        })
        .unwrap();
        assert_eq!(args[2], "-o");
        assert!(args[3].starts_with(DEFAULT_MOCK_OPTIONS));
        assert!(args[3].ends_with(" --uniqueext=review"), "{}", args[3]);
    }

    #[test]
    fn build_args_appends_arbitrary_mock_options() {
        let args = build_args(&RunSpec {
            mock_options: vec!["--isolation=simple".to_string(), "--nocheck".to_string()],
            ..spec("1")
        })
        .unwrap();
        assert!(
            args[3].ends_with(" --isolation=simple --nocheck"),
            "{}",
            args[3]
        );
    }

    #[test]
    fn build_args_rejects_whitespace_in_o_entries() {
        // fedora-review re-splits the -o string on whitespace, so a
        // spaced entry would silently become two mock arguments.
        assert!(
            build_args(&RunSpec {
                uniqueext: Some("a b".to_string()),
                ..spec("1")
            })
            .is_err()
        );
        assert!(
            build_args(&RunSpec {
                uniqueext: Some(String::new()),
                ..spec("1")
            })
            .is_err()
        );
        assert!(
            build_args(&RunSpec {
                mock_options: vec!["--plugin-option a".to_string()],
                ..spec("1")
            })
            .is_err()
        );
    }

    #[test]
    fn build_args_rejects_non_numeric_bug() {
        assert!(build_args(&spec("abc")).is_err());
        assert!(build_args(&spec("")).is_err());
    }

    #[test]
    fn display_command_quotes_spaced_args() {
        let args = vec![
            "-b".to_string(),
            "1".to_string(),
            "-o".to_string(),
            "a b".to_string(),
        ];
        assert_eq!(display_command(&args), "fedora-review -b 1 -o 'a b'");
    }
}
