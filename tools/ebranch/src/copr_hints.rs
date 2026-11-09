// SPDX-License-Identifier: Apache-2.0 OR MIT

//! What the change owner's COPR knows about the packages left to
//! rebuild (#61).
//!
//! A library's change owner often trials the dependents' rebuilds in a
//! COPR before the side tag exists (the libical 4 one in
//! `mcrha/libical-4.x`). That COPR already knows which packages needed
//! source changes: the patches its specs carry that dist-git's do not.
//! And a build there of an older version than the branch's says the
//! trial no longer matches what has to be built. Both are read here,
//! read only, from COPR's API and its SRPMs.
//!
//! The SRPMs are untrusted input: only the spec and the patches are
//! extracted (`rpm2cpio | cpio`), into a scratch directory removed
//! afterwards, and nothing in them is built or evaluated. The spec they
//! are compared with is dist-git's, fetched from dist-git.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use serde::Serialize;

/// What the COPR says about one package left to rebuild.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CoprHint {
    pub package: String,
    /// The COPR's newest succeeded build, `V-R`.
    pub copr_version: String,
    /// The branch's version, from dist-git's spec, when it is written
    /// out there rather than through a macro.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch_version: Option<String>,
    /// Whether the COPR built an older version than the branch has: its
    /// patches are a hint only, made for other sources.
    pub older: bool,
    /// Patches the COPR's spec carries that dist-git's does not.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub carried: Vec<CarriedPatch>,
    /// Why the patches could not be compared, when they could not.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unchecked: Option<String>,
}

/// A patch the COPR carries, and where it says it came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CarriedPatch {
    pub file: String,
    /// The spec's comment just above its `Patch:` line.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_url: Option<String>,
}

/// One `Patch:` line of a spec: the file it names, and the comment just
/// above it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecPatch {
    pub file: String,
    pub comment: Option<String>,
}

/// The `Patch:`/`PatchN:` lines of `spec`, each with the comment lines
/// directly above it. The file is the value's last path segment, so a
/// URL and a bare name compare alike.
pub fn spec_patches(spec: &str) -> Vec<SpecPatch> {
    let mut out = Vec::new();
    let mut comment: Vec<String> = Vec::new();
    for line in spec.lines() {
        let t = line.trim();
        if let Some(c) = t.strip_prefix('#') {
            comment.push(c.trim().to_string());
            continue;
        }
        let tag = t.split(':').next().unwrap_or("");
        let is_patch = tag.len() >= 5
            && tag[..5].eq_ignore_ascii_case("patch")
            && tag[5..].chars().all(|c| c.is_ascii_digit());
        if is_patch && let Some((_, value)) = t.split_once(':') {
            // The file is the value's first word: anything after it is
            // not part of the name.
            let value = value.split_whitespace().next().unwrap_or("");
            let file = value.rsplit('/').next().unwrap_or(value).to_string();
            if !file.is_empty() {
                out.push(SpecPatch {
                    file,
                    comment: (!comment.is_empty()).then(|| comment.join(" ")),
                });
            }
        }
        comment.clear();
    }
    out
}

/// The spec's `Version:`, when it is written out rather than a macro.
pub fn spec_version(spec: &str) -> Option<String> {
    spec.lines().find_map(|l| {
        let (tag, v) = l.trim().split_once(':')?;
        let v = v.trim();
        (tag.trim().eq_ignore_ascii_case("version") && !v.is_empty() && !v.contains('%'))
            .then(|| v.to_string())
    })
}

/// A `git format-patch` header's `From:`, `Subject:` and `X-Git-Url:`.
pub fn patch_header(text: &str) -> (Option<String>, Option<String>, Option<String>) {
    let mut from = None;
    let mut subject = None;
    let mut url = None;
    for line in text.lines().take(40) {
        if line.starts_with("diff ") || line.starts_with("--- ") {
            break;
        }
        let field = |prefix: &str| line.strip_prefix(prefix).map(|v| v.trim().to_string());
        from = from.or(field("From: ").map(|f| decode_header(&f)));
        subject = subject.or(field("Subject: ").map(|s| decode_header(&s)).map(|s| {
            s.strip_prefix("[PATCH")
                .and_then(|r| r.split_once(']'))
                .map_or(s.clone(), |(_, rest)| rest.trim().to_string())
        }));
        url = url.or(field("X-Git-Url: "));
    }
    (from, subject, url)
}

/// A header value with its RFC 2047 encoded words (`=?UTF-8?q?…?=`, as
/// `git format-patch` writes a non-ASCII name) decoded; Q encoding only,
/// anything else left as it is.
pub fn decode_header(value: &str) -> String {
    let mut out = String::new();
    let mut rest = value;
    while let Some(start) = rest.find("=?") {
        let after = &rest[start + 2..];
        let parts: Vec<&str> = after.splitn(3, '?').collect();
        let decoded = match parts.as_slice() {
            [charset, enc, tail]
                if charset.eq_ignore_ascii_case("utf-8") && enc.eq_ignore_ascii_case("q") =>
            {
                tail.find("?=").map(|end| (q_decode(&tail[..end]), end))
            }
            _ => None,
        };
        match decoded {
            Some((text, end)) => {
                out.push_str(&rest[..start]);
                out.push_str(&text);
                let consumed = 2 + parts[0].len() + 1 + parts[1].len() + 1 + end + 2;
                rest = &rest[start + consumed..];
                // Space between two encoded words is not part of either.
                let trimmed = rest.trim_start_matches([' ', '\t']);
                if trimmed.starts_with("=?") {
                    rest = trimmed;
                }
            }
            None => {
                out.push_str(&rest[..start + 2]);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// One Q-encoded word's text: `_` is a space, `=XX` a byte.
fn q_decode(word: &str) -> String {
    let mut bytes = Vec::new();
    let mut chars = word.bytes().peekable();
    while let Some(b) = chars.next() {
        match b {
            b'_' => bytes.push(b' '),
            b'=' => {
                let hex: Vec<u8> = chars.by_ref().take(2).collect();
                match std::str::from_utf8(&hex)
                    .ok()
                    .and_then(|h| u8::from_str_radix(h, 16).ok())
                {
                    Some(v) => bytes.push(v),
                    None => {
                        bytes.push(b'=');
                        bytes.extend(hex);
                    }
                }
            }
            other => bytes.push(other),
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

/// What a patch changes, for telling one renamed in dist-git from one
/// it lacks: its `Subject:` when it has one, else its diff lines.
pub fn patch_identity(text: &str) -> String {
    match patch_header(text).1 {
        Some(subject) => format!("subject:{}", subject.trim().to_lowercase()),
        None => {
            let diff: Vec<&str> = text
                .lines()
                .filter(|l| {
                    (l.starts_with('+') || l.starts_with('-'))
                        && !l.starts_with("+++")
                        && !l.starts_with("---")
                })
                .collect();
            format!("diff:{}", diff.join("\n"))
        }
    }
}

/// The patches `copr` carries that `distgit` does not, by file name.
pub fn carried<'a>(copr: &'a [SpecPatch], distgit: &[SpecPatch]) -> Vec<&'a SpecPatch> {
    copr.iter()
        .filter(|p| !distgit.iter().any(|d| d.file == p.file))
        .collect()
}

/// `V` of an `E:V-R`.
fn version_of(evr: &str) -> &str {
    let vr = evr.split_once(':').map_or(evr, |(_, r)| r);
    vr.rsplit_once('-').map_or(vr, |(v, _)| v)
}

/// The spec and the patches of the SRPM in `srpm`, extracted into `dir`
/// and nothing else: `(spec text, patch file name → text)`.
fn unpack(srpm: &[u8], dir: &Path) -> Result<(String, BTreeMap<String, String>), String> {
    let mut rpm2cpio = Command::new("rpm2cpio")
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("cannot run rpm2cpio: {e}"))?;
    let mut stdin = rpm2cpio.stdin.take().ok_or("rpm2cpio has no stdin")?;
    let data = srpm.to_vec();
    let feeder = std::thread::spawn(move || stdin.write_all(&data));
    let cpio = Command::new("cpio")
        .args(["-idm", "--quiet", "--no-absolute-filenames"])
        .args(["*.spec", "*.patch", "*.diff"])
        .current_dir(dir)
        .stdin(rpm2cpio.stdout.take().ok_or("rpm2cpio has no stdout")?)
        .stderr(Stdio::null())
        .status()
        .map_err(|e| format!("cannot run cpio: {e}"))?;
    let _ = feeder.join();
    let _ = rpm2cpio.wait();
    if !cpio.success() {
        return Err("the SRPM could not be unpacked".to_string());
    }
    let mut spec = None;
    let mut patches = BTreeMap::new();
    for entry in std::fs::read_dir(dir).map_err(|e| e.to_string())? {
        let path = entry.map_err(|e| e.to_string())?.path();
        // Only regular files directly in the directory.
        if !path.is_file() || path.is_symlink() {
            continue;
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let text =
            String::from_utf8_lossy(&std::fs::read(&path).map_err(|e| e.to_string())?).into_owned();
        if name.ends_with(".spec") {
            spec = Some(text);
        } else {
            patches.insert(name, text);
        }
    }
    Ok((spec.ok_or("the SRPM has no spec")?, patches))
}

/// What the COPR `owner/project` says about each of `packages` (those
/// left to rebuild on `branch`) that it has a succeeded build of.
/// Lookups that fail are returned as `(package, reason)` for the
/// report's gaps.
pub fn hints(
    copr_ref: &str,
    packages: &[String],
    branch: &str,
    verbose: bool,
) -> (Vec<CoprHint>, Vec<(Option<String>, String)>) {
    let mut gaps = Vec::new();
    let Some((owner, project)) = copr_ref.split_once('/') else {
        gaps.push((None, format!("{copr_ref} is not <owner>/<project>")));
        return (Vec::new(), gaps);
    };
    let copr = sandogasa_copr::Copr::new();
    if verbose {
        eprintln!("[ebranch] reading COPR {copr_ref}");
    }
    let listed = match copr.monitor(owner, project) {
        Ok(l) => l,
        Err(e) => {
            gaps.push((None, format!("COPR {copr_ref}: {e}")));
            return (Vec::new(), gaps);
        }
    };
    let prefix = sandogasa_copr::chroot_prefix(branch);
    let http = match reqwest::blocking::Client::builder()
        .user_agent(concat!(
            env!("CARGO_PKG_NAME"),
            "/",
            env!("CARGO_PKG_VERSION")
        ))
        .build()
    {
        Ok(h) => h,
        Err(e) => {
            gaps.push((None, e.to_string()));
            return (Vec::new(), gaps);
        }
    };
    let mut out = Vec::new();
    for package in packages {
        let Some(status) = listed.iter().find(|p| &p.name == package) else {
            continue;
        };
        // The branch's chroot when it has one, else any, succeeded.
        let pick = status
            .chroots
            .iter()
            .filter(|(_, s)| s.state == "succeeded" && s.build_id.is_some())
            .max_by_key(|(c, _)| prefix.as_deref().is_some_and(|p| c.starts_with(p)));
        let Some((chroot, state)) = pick else {
            continue;
        };
        let copr_version = state.pkg_version.clone().unwrap_or_default();
        let distgit_spec = http
            .get(format!(
                "https://src.fedoraproject.org/rpms/{package}/raw/{branch}/f/{package}.spec"
            ))
            .send()
            .and_then(|r| r.error_for_status())
            .and_then(|r| r.text());
        let distgit_spec = match distgit_spec {
            Ok(s) => s,
            Err(e) => {
                gaps.push((Some(package.clone()), format!("dist-git spec: {e}")));
                continue;
            }
        };
        let branch_version = spec_version(&distgit_spec);
        let older = branch_version.as_deref().is_some_and(|b| {
            sandogasa_rpmvercmp::rpmvercmp(version_of(&copr_version), b) == std::cmp::Ordering::Less
        });
        let mut hint = CoprHint {
            package: package.clone(),
            copr_version,
            branch_version,
            older,
            carried: Vec::new(),
            unchecked: None,
        };
        match srpm_of(&copr, owner, project, chroot, state.build_id.unwrap_or(0)) {
            Ok(Some(srpm)) => match tempfile::tempdir()
                .map_err(|e| e.to_string())
                .and_then(|dir| unpack(&srpm, dir.path()))
            {
                Ok((spec, patches)) => {
                    let copr_patches = spec_patches(&spec);
                    let ours = spec_patches(&distgit_spec);
                    let candidates = carried(&copr_patches, &ours);
                    // A patch dist-git has under another name is not
                    // carried: compare what each changes.
                    let distgit_ids: Vec<String> = if candidates.is_empty() {
                        Vec::new()
                    } else {
                        ours.iter()
                            .filter_map(|d| {
                                http.get(format!(
                                    "https://src.fedoraproject.org/rpms/{package}/raw/{branch}/f/{}",
                                    d.file
                                ))
                                .send()
                                .and_then(|r| r.error_for_status())
                                .and_then(|r| r.text())
                                .ok()
                            })
                            .map(|t| patch_identity(&t))
                            .collect()
                    };
                    for p in candidates {
                        if patches
                            .get(&p.file)
                            .is_some_and(|t| distgit_ids.contains(&patch_identity(t)))
                        {
                            continue;
                        }
                        let (from, subject, git_url) = patches
                            .get(&p.file)
                            .map(|t| patch_header(t))
                            .unwrap_or_default();
                        hint.carried.push(CarriedPatch {
                            file: p.file.clone(),
                            comment: p.comment.clone(),
                            from,
                            subject,
                            git_url,
                        });
                    }
                }
                Err(e) => hint.unchecked = Some(e),
            },
            Ok(None) => hint.unchecked = Some("SRPM no longer available".to_string()),
            Err(e) => hint.unchecked = Some(e),
        }
        out.push(hint);
    }
    (out, gaps)
}

/// The SRPM of build `id`, from Pulp; `None` when it is gone.
fn srpm_of(
    copr: &sandogasa_copr::Copr,
    owner: &str,
    project: &str,
    chroot: &str,
    id: u64,
) -> Result<Option<Vec<u8>>, String> {
    let build = copr.build(id).map_err(|e| e.to_string())?;
    let Some(name) = build
        .source_package
        .url
        .as_deref()
        .and_then(|u| u.rsplit('/').next())
        .filter(|n| n.ends_with(".src.rpm"))
    else {
        return Ok(None);
    };
    copr.download(&sandogasa_copr::srpm_url(owner, project, chroot, name))
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const COPR_SPEC: &str = "\
Name: orage
Version: 4.20.1
Release: %autorelease.1
Patch0: orage-desktop.patch
# Backport of upstream 1ad84821: build against libical 4
Patch1: https://gitlab.xfce.org/apps/orage/-/commit/1ad84821.patch
# from openmamba
Patch2: libical4-port.patch
BuildRequires: libical-devel >= 4.0.5
";

    const DISTGIT_SPEC: &str = "\
Name: orage
Version: %{majorver}.1
Patch0: orage-desktop.patch
";

    #[test]
    fn the_copr_carries_the_patches_dist_git_lacks_with_their_comments() {
        let copr = spec_patches(COPR_SPEC);
        assert_eq!(copr.len(), 3);
        let extra = carried(&copr, &spec_patches(DISTGIT_SPEC));
        assert_eq!(
            extra.iter().map(|p| p.file.as_str()).collect::<Vec<_>>(),
            ["1ad84821.patch", "libical4-port.patch"]
        );
        assert_eq!(
            extra[0].comment.as_deref(),
            Some("Backport of upstream 1ad84821: build against libical 4")
        );
        assert_eq!(extra[1].comment.as_deref(), Some("from openmamba"));
        // A comment not directly above a Patch line is not its comment.
        assert_eq!(copr[0].comment, None);
    }

    #[test]
    fn a_literal_version_is_read_and_a_macro_is_not() {
        assert_eq!(spec_version(COPR_SPEC).as_deref(), Some("4.20.1"));
        assert_eq!(spec_version(DISTGIT_SPEC), None);
        assert_eq!(version_of("1:23.4.1-2"), "23.4.1");
        assert_eq!(version_of("3.7.1-1"), "3.7.1");
    }

    #[test]
    fn a_patch_file_is_the_values_first_word() {
        let p = spec_patches("Patch01: gb-309138-fix.patch as this\n");
        assert_eq!(p[0].file, "gb-309138-fix.patch");
    }

    #[test]
    fn an_encoded_name_is_decoded() {
        assert_eq!(
            decode_header("=?UTF-8?q?Michal=20Josef=20=C5=A0pa=C4=8Dek?= <mspacek@redhat.com>"),
            "Michal Josef Špaček <mspacek@redhat.com>"
        );
        assert_eq!(
            decode_header("Plain Name <p@example.org>"),
            "Plain Name <p@example.org>"
        );
        assert_eq!(
            decode_header("=?ISO-8859-1?b?QQ==?= x"),
            "=?ISO-8859-1?b?QQ==?= x"
        );
    }

    /// asterisk's dist-git carries the COPR's patches renamed: the same
    /// change is not a carried one.
    #[test]
    fn a_renamed_patch_is_the_same_change() {
        let copr = "From: Jared Smith <jsmith@fedoraproject.org>\n\
                    Subject: [PATCH] Update from python2 to python3 for various scripts\n\n";
        let distgit = "From 1 Mon\nFrom: Jared Smith <jsmith@fedoraproject.org>\n\
                       Subject: [PATCH 1/2] Update from python2 to python3 for various scripts\n";
        assert_eq!(patch_identity(copr), patch_identity(distgit));
        let bare_a = "--- a/x\n+++ b/x\n-old\n+new\n";
        let bare_b = "diff -u x x\n--- a/x\n+++ b/x\n-old\n+new\n";
        assert_eq!(patch_identity(bare_a), patch_identity(bare_b));
        assert_ne!(patch_identity(bare_a), patch_identity("+other\n"));
    }

    #[test]
    fn a_patch_says_who_wrote_it_and_where_from() {
        let text = "From 1ad84821 Mon Sep 17 00:00:00 2001\n\
                    From: Some One <one@example.org>\n\
                    Date: Mon, 3 Nov 2026 10:00:00 +0100\n\
                    Subject: [PATCH 2/3] Build against libical 4\n\
                    X-Git-Url: https://gitlab.xfce.org/apps/orage/-/commit/1ad84821\n\
                    \n\
                    ---\n\
                    diff --git a/x b/x\n\
                    From: not a header\n";
        let (from, subject, url) = patch_header(text);
        assert_eq!(from.as_deref(), Some("Some One <one@example.org>"));
        assert_eq!(subject.as_deref(), Some("Build against libical 4"));
        assert_eq!(
            url.as_deref(),
            Some("https://gitlab.xfce.org/apps/orage/-/commit/1ad84821")
        );
        assert_eq!(patch_header("just a diff\n"), (None, None, None));
    }
}
