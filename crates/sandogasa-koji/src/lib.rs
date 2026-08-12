// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Koji build system CLI wrapper.
//!
//! Provides functions for querying Koji tags and builds by shelling
//! out to the `koji` CLI. Supports multiple Koji profiles (e.g.
//! `cbs` for CentOS Build System).

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::Duration;
use wait_timeout::ChildExt;

/// How long a koji call may take before it is treated as a hub that is
/// not answering.
///
/// The koji CLI waits indefinitely, so a hub outage — mass branching,
/// an unplanned one — hung every caller with no output at all rather
/// than degrading to what was already known. Thirty seconds is well
/// above a slow-but-working hub: `list-builds` on a long-lived package
/// answers in a few.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Environment variable overriding [`DEFAULT_TIMEOUT`], in seconds. A
/// value of `0` waits forever, which is the old behaviour and what a
/// caller genuinely willing to block should ask for.
pub const TIMEOUT_ENV: &str = "SANDOGASA_KOJI_TIMEOUT";

/// Profiles whose hub has already failed to answer in this process.
///
/// A hub that did not answer once will not answer the next call either,
/// and every caller here asks many times — one query per tag, per
/// package. Paying the timeout for each turned a 30-second bound into
/// minutes of waiting for a report that was always going to come from
/// the ledger. So the first timeout stands for the rest of the run.
static UNRESPONSIVE: std::sync::Mutex<Option<std::collections::BTreeSet<String>>> =
    std::sync::Mutex::new(None);

fn profile_key(profile: Option<&str>) -> String {
    profile.unwrap_or_default().to_string()
}

/// Whether this profile's hub has already failed to answer, so a caller
/// with many queries left can stop and report from what it has instead
/// of timing out once per query.
pub fn hub_unresponsive(profile: Option<&str>) -> bool {
    UNRESPONSIVE
        .lock()
        .map(|set| {
            set.as_ref()
                .is_some_and(|set| set.contains(&profile_key(profile)))
        })
        .unwrap_or(false)
}

/// Whether an error from this crate says the tag does not exist, as
/// opposed to saying nothing definite.
///
/// The distinction decides whether a caller may act on absence — drop a
/// side tag from a ledger, say — and only the hub answering "no such
/// tag" licenses that. A timeout, an unreachable hub or an unparseable
/// failure must never be read as absence: acting on those would erase
/// records during an outage, which is when they matter most.
///
/// Matched on the CLI's message ("No such tag: <name>", observed
/// 2026-08-11) because the koji CLI reports it no other way — it exits
/// non-zero for every kind of failure alike. Kept here so the knowledge
/// sits in one place rather than at each call site.
pub fn tag_missing(error: &str) -> bool {
    !error.contains("did not answer within")
        && !error.contains("skipped: the hub did not answer")
        && error.to_ascii_lowercase().contains("no such tag")
}

fn mark_unresponsive(profile: Option<&str>) {
    if let Ok(mut set) = UNRESPONSIVE.lock() {
        set.get_or_insert_with(Default::default)
            .insert(profile_key(profile));
    }
}

fn timeout() -> Option<Duration> {
    match std::env::var(TIMEOUT_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
    {
        Some(0) => None,
        Some(secs) => Some(Duration::from_secs(secs)),
        None => Some(DEFAULT_TIMEOUT),
    }
}

/// A build found in a Koji tag.
#[derive(Debug, Clone)]
pub struct TaggedBuild {
    /// Name-Version-Release.
    pub nvr: String,
    /// Tag the build is in.
    pub tag: String,
    /// FAS username of the builder.
    pub owner: String,
}

/// Parse an NVR string into (name, version, release).
///
/// ```
/// let (n, v, r) = sandogasa_koji::parse_nvr("systemd-256.12-1.fc42").unwrap();
/// assert_eq!(n, "systemd");
/// assert_eq!(v, "256.12");
/// assert_eq!(r, "1.fc42");
/// ```
pub fn parse_nvr(nvr: &str) -> Option<(&str, &str, &str)> {
    let mut parts = nvr.rsplitn(3, '-');
    let release = parts.next()?;
    let version = parts.next()?;
    let name = parts.next()?;
    if name.is_empty() {
        None
    } else {
        Some((name, version, release))
    }
}

/// Parse an NVR string into the package name.
///
/// Returns `None` if the NVR doesn't contain at least two hyphens.
///
/// ```
/// assert_eq!(sandogasa_koji::parse_nvr_name("systemd-256.12-1.fc42"), Some("systemd"));
/// assert_eq!(sandogasa_koji::parse_nvr_name("intel-gpu-tools-1.28-2.el10"), Some("intel-gpu-tools"));
/// assert_eq!(sandogasa_koji::parse_nvr_name("nohyphens"), None);
/// ```
pub fn parse_nvr_name(nvr: &str) -> Option<&str> {
    parse_nvr(nvr).map(|(name, _, _)| name)
}

/// Whether the `koji` CLI is available on PATH. Callers that can
/// degrade gracefully should probe this once up front and warn,
/// rather than erroring on every query. Probes with `koji help`:
/// it's the offline no-op — `--version` doesn't exist (exit 2)
/// and `koji version` contacts the hub.
pub fn is_available() -> bool {
    let mut cmd = Command::new("koji");
    cmd.arg("help");
    // `koji help` is offline, so a failure here means koji is missing
    // rather than that the hub is unreachable — it must not latch.
    run_bounded_with(cmd, "help", timeout()).is_ok()
}

/// Run `cmd`, returning its stdout, and give up on it if it outlasts
/// [`timeout`].
///
/// Both streams are drained on their own threads rather than read after
/// the wait: a pipe holds 64 KiB, and `list-tagged` on a release tag
/// runs to megabytes, so a child blocked writing to a full pipe would
/// be indistinguishable from a hung hub and killed as one.
fn run_bounded(cmd: Command, label: &str, profile: Option<&str>) -> Result<String, String> {
    if hub_unresponsive(profile) {
        return Err(format!(
            "koji {label} skipped: the hub did not answer an earlier call in this run"
        ));
    }
    let result = run_bounded_with(cmd, label, timeout());
    if let Err(e) = &result
        && e.contains("did not answer within")
    {
        mark_unresponsive(profile);
    }
    result
}

/// [`run_bounded`] with the limit passed in rather than read from the
/// environment, so a test can set one without racing every other test
/// in the process.
fn run_bounded_with(
    mut cmd: Command,
    label: &str,
    limit: Option<Duration>,
) -> Result<String, String> {
    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to run koji: {e}"))?;
    let drain = |stream: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut text = String::new();
            if let Some(mut stream) = stream {
                let _ = stream.read_to_string(&mut text);
            }
            text
        })
    };
    let out = drain(
        child
            .stdout
            .take()
            .map(|s| Box::new(s) as Box<dyn Read + Send>),
    );
    let err = drain(
        child
            .stderr
            .take()
            .map(|s| Box::new(s) as Box<dyn Read + Send>),
    );

    let status = match limit {
        Some(limit) => match child.wait_timeout(limit).map_err(|e| e.to_string())? {
            Some(status) => status,
            None => {
                // Killed rather than left behind: the caller is going to
                // report and carry on, and an abandoned koji process
                // would go on holding a connection nobody is waiting for.
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "koji {label} did not answer within {}s; the hub may be down (override with {TIMEOUT_ENV})",
                    limit.as_secs()
                ));
            }
        },
        None => child.wait().map_err(|e| e.to_string())?,
    };
    let stdout = out.join().unwrap_or_default();
    let stderr = err.join().unwrap_or_default();
    if !status.success() {
        return Err(format!("koji {label} failed: {}", stderr.trim()));
    }
    Ok(stdout)
}

/// Run a koji command with optional profile and return stdout.
fn run_koji(profile: Option<&str>, args: &[&str]) -> Result<String, String> {
    let mut cmd = Command::new("koji");
    if let Some(p) = profile {
        cmd.args(["--profile", p]);
    }
    cmd.args(args);
    run_bounded(cmd, args.first().unwrap_or(&""), profile)
}

/// List builds in a Koji tag with their owners.
///
/// Returns the NVR, tag, and owner for each build.
/// Uses `--latest` to only show the latest build of each package.
/// If `timestamp` is given, queries the tag state at that Unix
/// timestamp.
pub fn list_tagged(
    tag: &str,
    profile: Option<&str>,
    timestamp: Option<i64>,
) -> Result<Vec<TaggedBuild>, String> {
    let ts_str = timestamp.map(|t| t.to_string());
    let mut args = vec!["list-tagged", "--latest"];
    if let Some(ref ts) = ts_str {
        args.push("--ts");
        args.push(ts);
    }
    // `--` so a tag starting with `-` can't be read as a flag.
    args.push("--");
    args.push(tag);
    let stdout = run_koji(profile, &args)?;
    Ok(parse_list_tagged(&stdout))
}

/// Latest build of `package` in `tag`, following tag inheritance
/// (`--inherit`), or `None` when the package has no build there.
///
/// Inheritance matters for "is this shipped in the release"
/// checks: a release's compose content is its tag chain (e.g.
/// `f43-updates` inherits `f43`, and the `rawhide` alias inherits
/// the current `fNN`), while side tags and `-candidate`/`-testing`
/// tags are never in the chain — so a build visible here is one
/// the release actually carries.
pub fn latest_tagged(
    tag: &str,
    package: &str,
    profile: Option<&str>,
) -> Result<Option<TaggedBuild>, String> {
    let stdout = run_koji(
        profile,
        &["list-tagged", "--latest", "--inherit", "--", tag, package],
    )?;
    Ok(parse_list_tagged(&stdout).into_iter().next())
}

/// Latest build of *every* package in `tag`, following inheritance.
///
/// The bulk form of [`latest_tagged`], for callers asking about enough
/// packages that one large answer beats many small ones. Measured
/// against the Fedora hub in August 2026: a release tag's candidate tag
/// answers in about 8 seconds with some 24,000 builds (1.7MB), while one
/// package costs about 1.25 seconds — so this pays from roughly seven
/// packages upward, and the caller is the one who knows how many it has.
pub fn latest_tagged_all(tag: &str, profile: Option<&str>) -> Result<Vec<TaggedBuild>, String> {
    let stdout = run_koji(
        profile,
        &["list-tagged", "--latest", "--inherit", "--", tag],
    )?;
    Ok(parse_list_tagged(&stdout))
}

/// Parse `koji list-tagged` tabular output (header, separator,
/// then `NVR TAG OWNER` rows) into builds.
fn parse_list_tagged(stdout: &str) -> Vec<TaggedBuild> {
    let mut builds = Vec::new();
    for line in stdout.lines().skip(2) {
        let line = line.trim();
        if line.is_empty() || line.starts_with('-') {
            continue;
        }
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 3 {
            builds.push(TaggedBuild {
                nvr: parts[0].to_string(),
                tag: parts[1].to_string(),
                owner: parts[2].to_string(),
            });
        }
    }
    builds
}

/// One "tagged into" event from `koji list-history --tag=<tag>`.
/// Captures only the bits the activity-reporting code needs;
/// the date column is parsed by the caller as needed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagAddEvent {
    /// `<weekday> <month> <day> <hh:mm:ss> <year>` as koji prints
    /// it under `--utc`. Kept as the original string so callers
    /// can preserve fidelity for display; parse with chrono if
    /// numeric comparisons are needed.
    pub when: String,
    /// The build NVR that was tagged.
    pub nvr: String,
    /// FAS username (or other Koji actor identifier) credited
    /// with the tagging.
    pub owner: String,
}

/// Walk a tag's "tagged into" events in the window `[after,
/// before)` and return them in chronological order. Implemented
/// via `koji list-history --tag=<tag>` parsed line-by-line; only
/// `tagged into` lines are kept (other event types like owner
/// changes and untags are filtered out).
///
/// Both bounds are dates: `after` is inclusive on the start,
/// `before` should be `until + 1 day` to make the window
/// closed on the `until` day. Times are in UTC.
pub fn tag_history(
    tag: &str,
    profile: Option<&str>,
    after: chrono::NaiveDate,
    before: chrono::NaiveDate,
) -> Result<Vec<TagAddEvent>, String> {
    let after_str = after.to_string();
    let before_str = before.to_string();
    let stdout = run_koji(
        profile,
        &[
            "list-history",
            "--tag",
            tag,
            "--after",
            &after_str,
            "--before",
            &before_str,
            "--utc",
        ],
    )?;
    Ok(parse_tag_history(&stdout))
}

/// Parse `koji list-history` output and pick out only the
/// "tagged into" entries. Format of those lines (as of koji
/// 1.34, `--utc`):
///
/// ```text
/// Thu Apr 30 23:19:02 2026 nvr-1.0-1.el10 tagged into mytag by user [still active]
/// ```
///
/// Five date/time tokens (weekday, month, day, hh:mm:ss, year),
/// then NVR, then `tagged into <tag> by <user>`, optionally
/// followed by `[still active]`. The tag name is already known
/// to the caller so we don't bother re-extracting it.
pub fn parse_tag_history(stdout: &str) -> Vec<TagAddEvent> {
    let mut out = Vec::new();
    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let parts: Vec<&str> = line.split_whitespace().collect();
        // Date prefix is 5 whitespace-separated tokens:
        //   <weekday> <month> <day> <hh:mm:ss> <year>
        // followed by <nvr> <tagged> <into> <tag-name> <by> <user> [...]
        if parts.len() < 11 {
            continue;
        }
        if parts[6] != "tagged" || parts[7] != "into" || parts[9] != "by" {
            continue;
        }
        let when = parts[0..5].join(" ");
        let nvr = parts[5].to_string();
        let owner = parts[10].to_string();
        out.push(TagAddEvent { when, nvr, owner });
    }
    out
}

/// Verify an authenticated Koji session is available for `profile`
/// by running `koji moshimoshi` (the authenticated hello).
///
/// Write operations (tag/untag) need authentication, which the
/// read-only queries don't — so callers run this up front, before
/// any expensive read-side work, to fail fast with an actionable
/// message instead of erroring at the first write. Returns the
/// koji error plus a profile-aware hint on how to authenticate.
pub fn check_auth(profile: Option<&str>) -> Result<(), String> {
    run_koji(profile, &["moshimoshi"]).map(|_| ()).map_err(|e| {
        let hint = match profile {
            Some("cbs") => "run `centos-cert` to obtain a CentOS client certificate",
            _ => "authenticate to Koji (e.g. `kinit`, or the profile's client cert)",
        };
        format!(
            "not authenticated to {} koji: {e}\n{hint}",
            profile.unwrap_or("the")
        )
    })
}

/// Tag a build into a Koji tag (`koji tag-build --wait <tag>
/// <nvr>`).
///
/// `--wait` is explicit because koji defaults to `--nowait` when
/// its stdout isn't a TTY (as when run as a subprocess): the tag
/// task would be queued and the command would return before the
/// build actually landed in the tag. Callers that tag-then-untag
/// (promoting a build from testing to release) rely on the tag
/// being confirmed first, so the build is never absent from both
/// tags.
///
/// Succeeds silently when Koji accepts the command; returns the
/// koji stderr otherwise. Koji tolerates re-tagging a build
/// already in the tag, so this is effectively idempotent.
pub fn tag_build(tag: &str, nvr: &str, profile: Option<&str>) -> Result<(), String> {
    run_koji(profile, &["tag-build", "--wait", "--", tag, nvr])?;
    Ok(())
}

/// Untag a build from a Koji tag (`koji untag-build <tag> <nvr>`).
///
/// Succeeds silently when Koji accepts the command; returns
/// the koji stderr otherwise. No-op on whether the build was
/// actually present beforehand — Koji tolerates re-untagging.
pub fn untag_build(tag: &str, nvr: &str, profile: Option<&str>) -> Result<(), String> {
    run_koji(profile, &["untag-build", "--", tag, nvr])?;
    Ok(())
}

/// Regenerate a Koji tag's repo (`koji regen-repo --wait <tag>`).
///
/// `--wait` is explicit because koji defaults to `--nowait` when
/// its stdout isn't a TTY (as when run as a subprocess): callers
/// re-query the regenerated repo immediately after this returns,
/// so the regen must actually have completed. Repo regeneration
/// can take several minutes on large tags.
pub fn regen_repo(tag: &str, profile: Option<&str>) -> Result<(), String> {
    run_koji(profile, &["regen-repo", "--wait", "--", tag])?;
    Ok(())
}

/// Fetch `koji buildinfo --changelog <nvr>` output verbatim for
/// display. Returns the raw stdout so callers can show it to a
/// human reviewing the build. Errors propagate (unlike the
/// date/rpm helpers, an unexpected failure here is worth
/// surfacing during an interactive review).
pub fn build_info_with_changelog(nvr: &str, profile: Option<&str>) -> Result<String, String> {
    run_koji(profile, &["buildinfo", "--changelog", "--", nvr])
}

/// List NVRs in a Koji tag (quiet mode, NVRs only).
pub fn list_tagged_nvrs(tag: &str, profile: Option<&str>) -> Result<Vec<String>, String> {
    let stdout = run_koji(profile, &["list-tagged", "--quiet", "--", tag])?;
    Ok(stdout
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| l.split_whitespace().next())
        .map(|s| s.to_string())
        .collect())
}

/// List the latest source package names tagged into a Koji tag.
///
/// Uses `--latest` so each package appears once (its newest build
/// in the tag), then reduces NVRs to source package names. Useful
/// for "which packages are currently shipped in this tag".
pub fn list_tagged_package_names(tag: &str, profile: Option<&str>) -> Result<Vec<String>, String> {
    let stdout = run_koji(profile, &["list-tagged", "--latest", "--quiet", "--", tag])?;
    let mut names: Vec<String> = stdout
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| l.split_whitespace().next())
        .filter_map(|nvr| parse_nvr_name(nvr).map(|s| s.to_string()))
        .collect();
    names.sort();
    names.dedup();
    Ok(names)
}

/// List Koji tag names matching a glob `pattern` (e.g.
/// `hyperscale10s-*-release`). Wraps `koji list-tags <pattern>`.
pub fn list_tags(pattern: &str, profile: Option<&str>) -> Result<Vec<String>, String> {
    let stdout = run_koji(profile, &["list-tags", "--", pattern])?;
    Ok(stdout
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|s| s.to_string())
        .collect())
}

/// Parse the build's creation date from `koji buildinfo`.
///
/// Looks for a `Creation time: YYYY-MM-DD HH:MM:SS` line and
/// returns the date portion. Returns `Ok(None)` if the line
/// isn't present or can't be parsed — callers should treat
/// that as "unknown date" rather than an error.
pub fn build_creation_date(
    nvr: &str,
    profile: Option<&str>,
) -> Result<Option<chrono::NaiveDate>, String> {
    let stdout = run_koji(profile, &["buildinfo", "--", nvr])?;
    for line in stdout.lines() {
        if let Some(rest) = line.strip_prefix("Creation time:") {
            let value = rest.trim();
            // Parse `YYYY-MM-DD HH:MM:SS` — take the date part.
            let date_part = value.split_whitespace().next().unwrap_or("");
            if let Ok(date) = chrono::NaiveDate::parse_from_str(date_part, "%Y-%m-%d") {
                return Ok(Some(date));
            }
        }
    }
    Ok(None)
}

/// List binary RPM names for a build via `koji buildinfo`.
///
/// Parses the RPMs section, returning binary package names
/// (excluding `.src.rpm` entries).
pub fn build_rpms(nvr: &str, profile: Option<&str>) -> Result<Vec<String>, String> {
    let stdout = run_koji(profile, &["buildinfo", "--", nvr])?;

    let mut in_rpms = false;
    let mut names = Vec::new();
    for line in stdout.lines() {
        if line.starts_with("RPMs:") {
            in_rpms = true;
            continue;
        }
        if !in_rpms {
            continue;
        }
        let path = line.split('\t').next().unwrap_or("").trim();
        if path.is_empty() {
            continue;
        }
        let filename = path.rsplit('/').next().unwrap_or(path);
        if filename.ends_with(".src.rpm") {
            continue;
        }
        if let Some(without_rpm) = filename.strip_suffix(".rpm")
            && let Some(dot_pos) = without_rpm.rfind('.')
            && let Some(name) = parse_nvr_name(&without_rpm[..dot_pos])
        {
            names.push(name.to_string());
        }
    }
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_list_tagged_rows() {
        let out = "Build                    Tag         Built by\n\
                   -----------------------  ----------  --------\n\
                   foo-1.2.0-1.fc45         f45         alice\n\
                   \n";
        let builds = parse_list_tagged(out);
        assert_eq!(builds.len(), 1);
        assert_eq!(builds[0].nvr, "foo-1.2.0-1.fc45");
        assert_eq!(builds[0].tag, "f45");
        assert_eq!(builds[0].owner, "alice");
    }

    #[test]
    fn parse_list_tagged_empty_result() {
        // Headers only — the shape koji prints for a package with
        // no build in the tag.
        let out = "Build                    Tag         Built by\n\
                   -----------------------  ----------  --------\n";
        assert!(parse_list_tagged(out).is_empty());
    }

    #[test]
    fn parse_nvr_name_standard() {
        assert_eq!(parse_nvr_name("systemd-256.12-1.fc42"), Some("systemd"));
    }

    #[test]
    fn parse_nvr_name_hyphenated() {
        assert_eq!(
            parse_nvr_name("intel-gpu-tools-1.28-2.el10"),
            Some("intel-gpu-tools")
        );
    }

    #[test]
    fn parse_nvr_name_too_short() {
        assert_eq!(parse_nvr_name("nohyphens"), None);
        assert_eq!(parse_nvr_name("one-hyphen"), None);
    }

    #[test]
    fn parse_nvr_full() {
        let (n, v, r) = parse_nvr("systemd-256.12-1.fc42").unwrap();
        assert_eq!(n, "systemd");
        assert_eq!(v, "256.12");
        assert_eq!(r, "1.fc42");
    }

    #[test]
    fn parse_nvr_full_hyphenated() {
        let (n, v, r) = parse_nvr("intel-gpu-tools-1.28-2.hs.el10").unwrap();
        assert_eq!(n, "intel-gpu-tools");
        assert_eq!(v, "1.28");
        assert_eq!(r, "2.hs.el10");
    }

    #[test]
    fn parse_nvr_full_too_short() {
        assert!(parse_nvr("nohyphens").is_none());
    }

    #[test]
    fn parse_tag_history_picks_only_tag_adds() {
        let stdout = "\
Thu Apr 30 23:19:02 2026 kpatch-0.9.11-0.4.hs.el10 tagged into hyperscale10s-packages-main-release by dcavalca [still active]
Wed May  6 20:04:12 2026 package owner dcavalca set for git-lfs in hyperscale10s-packages-main-release by dcavalca [still active]
Thu May  7 16:22:29 2026 git-lfs-3.7.1-5.20260423gite09c0f6.hs.el10 tagged into hyperscale10s-packages-main-release by dcavalca [still active]
Mon May 18 15:35:11 2026 ethtool-6.14-1.hs.el10 untagged from hyperscale10s-packages-main-release by salimma
";
        let events = parse_tag_history(stdout);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].nvr, "kpatch-0.9.11-0.4.hs.el10");
        assert_eq!(events[0].owner, "dcavalca");
        assert_eq!(events[0].when, "Thu Apr 30 23:19:02 2026");
        assert_eq!(events[1].nvr, "git-lfs-3.7.1-5.20260423gite09c0f6.hs.el10");
        assert_eq!(events[1].owner, "dcavalca");
    }

    #[test]
    fn parse_tag_history_ignores_owner_and_pkg_list_entries() {
        // These three look superficially similar to a tag event
        // but use other verbs in the same positions.
        let stdout = "\
Wed May  6 20:04:12 2026 package owner dcavalca set for git-lfs in hyperscale10s-packages-main-release by dcavalca [still active]
Wed May  6 20:04:12 2026 package list entry created: git-lfs in hyperscale10s-packages-main-release by dcavalca [still active]
Mon May 18 15:35:11 2026 ethtool-6.14-1.hs.el10 untagged from hyperscale10s-packages-main-release by salimma
";
        assert!(parse_tag_history(stdout).is_empty());
    }

    #[test]
    fn parse_tag_history_handles_blank_lines() {
        let stdout =
            "\n\nThu Apr 30 23:19:02 2026 foo-1-1.el10 tagged into bar by u [still active]\n\n";
        let events = parse_tag_history(stdout);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].nvr, "foo-1-1.el10");
    }

    /// The bounded runner is exercised through `sh`, not `koji`: the
    /// tests have to pass in a packaging sandbox with no hub to talk to
    /// and no koji installed.
    #[test]
    fn bounded_run_returns_stdout() {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "printf 'one\ntwo\n'"]);
        let out = run_bounded_with(cmd, "test", Some(Duration::from_secs(20))).unwrap();
        assert_eq!(out, "one\ntwo\n");
    }

    #[test]
    fn bounded_run_reports_a_failure_with_its_stderr() {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "echo 'no such tag' >&2; exit 1"]);
        let err = run_bounded_with(cmd, "list-tagged", Some(Duration::from_secs(20))).unwrap_err();
        assert!(err.contains("list-tagged failed"), "{err}");
        assert!(err.contains("no such tag"), "{err}");
    }

    #[test]
    fn bounded_run_gives_up_on_a_command_that_does_not_answer() {
        // What a hung hub looks like: koji itself waits forever.
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "sleep 30"]);
        let started = std::time::Instant::now();
        let err = run_bounded_with(cmd, "list-tagged", Some(Duration::from_secs(1))).unwrap_err();
        assert!(err.contains("did not answer within 1s"), "{err}");
        assert!(err.contains(TIMEOUT_ENV), "{err}");
        // Returned on the timeout rather than after the child's own
        // 30 seconds, so the caller really is released.
        assert!(started.elapsed() < std::time::Duration::from_secs(10));
    }

    #[test]
    fn bounded_run_survives_more_output_than_a_pipe_holds() {
        // A pipe holds 64 KiB. Read after the wait rather than during
        // it, a child blocked writing this would look exactly like a
        // hub that never answered, and be killed as one.
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "seq 1 200000"]);
        let out = run_bounded_with(cmd, "list-tagged", Some(Duration::from_secs(20))).unwrap();
        assert!(out.len() > 1_000_000, "{} bytes", out.len());
        assert!(out.ends_with("200000\n"));
    }

    #[test]
    fn only_the_hub_saying_so_means_a_tag_is_missing() {
        assert!(tag_missing(
            "koji list-tagged failed: No such tag: f43-build-side-1"
        ));
        // Case as the hub happens to write it.
        assert!(tag_missing("no such tag: f43-build-side-1"));
        // Nothing definite: acting on these would drop a live tag
        // because the hub was down.
        assert!(!tag_missing(
            "koji list-tagged did not answer within 30s; the hub may be down"
        ));
        assert!(!tag_missing(
            "koji list-tagged skipped: the hub did not answer an earlier call in this run"
        ));
        assert!(!tag_missing(
            "koji list-tagged failed: authentication error"
        ));
        assert!(!tag_missing(
            "failed to run koji: No such file or directory"
        ));
    }
}
