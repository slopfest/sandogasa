// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Whether stock carries the patches a Proposed Update adds (#89).
//!
//! What an update adds is read from its merge request's diff: the
//! `.patch` files it creates, and the `PatchN:` lines it adds to the
//! spec. Each is matched against stock's patches by the upstream commit
//! it carries (the `From <sha>` line `git format-patch` writes, or the
//! commit a `PatchN:` URL names), or, without one, by its change itself
//! (its added and removed lines, as Red Hat may rename the file). Jira
//! cannot settle this: PackageKit's fix reached stock through Red Hat's
//! own commit while the SIG's RHEL issue read `Not a Bug`, and
//! blktrace's issues read `Done` while stock carried nothing.

/// A patch file, by name and content.
#[derive(Debug, Clone, PartialEq)]
pub struct PatchFile {
    pub name: String,
    pub text: String,
}

/// What a comparison found: each patch the update adds that stock
/// carries, with how it matched, and each it does not.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Comparison {
    /// `(the update's patch, stock's patch, how)`.
    pub matched: Vec<(String, String, String)>,
    pub missing: Vec<String>,
}

impl Comparison {
    /// Every patch the update adds is in stock.
    pub fn all_carried(&self) -> bool {
        self.missing.is_empty() && !self.matched.is_empty()
    }
}

/// The upstream commits `git format-patch` output names, from its
/// `From <40-hex> Mon Sep 17 00:00:00 2001` lines (one per commit in a
/// series).
pub fn format_patch_shas(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|l| l.strip_prefix("From "))
        .filter_map(|rest| rest.split_once(' '))
        .filter(|(sha, date)| sha.len() == 40 && is_hex(sha) && date.contains("2001"))
        .map(|(sha, _)| sha.to_ascii_lowercase())
        .collect()
}

/// The commit a patch URL names: GitHub and GitLab `/commit/<sha>`
/// (and `.patch` or `.diff` after it), `/commits/<sha>`, and cgit's
/// `id=<sha>`. At least seven hex digits, as a short id may be.
pub fn url_sha(url: &str) -> Option<String> {
    let after = ["/commit/", "/commits/", "id=", "h="]
        .iter()
        .find_map(|m| url.split_once(m).map(|(_, rest)| rest))?;
    let sha: String = after
        .chars()
        .take_while(|c| c.is_ascii_hexdigit())
        .collect();
    (sha.len() >= 7).then(|| sha.to_ascii_lowercase())
}

/// The commits a spec's `PatchN:` (or `Patch:`) lines name by URL.
pub fn spec_patch_shas(spec: &str) -> Vec<String> {
    spec.lines()
        .filter_map(patch_line_url)
        .filter_map(url_sha)
        .collect()
}

/// The patches a merge request adds, from its diffs: each new `.patch`
/// file, its content taken from the diff's added lines.
pub fn added_patches(diffs: &[sandogasa_gitlab::MergeRequestDiff]) -> Vec<PatchFile> {
    diffs
        .iter()
        .filter(|d| d.new_file && d.new_path.ends_with(".patch"))
        .map(|d| PatchFile {
            name: d.new_path.clone(),
            text: d
                .diff
                .lines()
                .filter_map(|l| l.strip_prefix('+'))
                .collect::<Vec<_>>()
                .join("\n"),
        })
        .collect()
}

/// The commits a merge request's spec changes name in `PatchN:` lines it
/// adds.
pub fn added_spec_shas(diffs: &[sandogasa_gitlab::MergeRequestDiff]) -> Vec<String> {
    diffs
        .iter()
        .filter(|d| d.new_path.ends_with(".spec"))
        .flat_map(|d| d.diff.lines())
        .filter_map(|l| l.strip_prefix('+'))
        .filter_map(patch_line_url)
        .filter_map(url_sha)
        .collect()
}

/// The change a patch makes, without what a rename or a re-export
/// alters: its added and removed lines, trailing whitespace dropped,
/// file headers, hunk positions and `git format-patch`'s signature (the
/// `-- ` line and the git version after it) left out. `None` when it
/// has none.
pub fn normalized_change(text: &str) -> Option<String> {
    let lines: Vec<&str> = text
        .lines()
        .take_while(|l| *l != "-- ")
        .filter(|l| {
            (l.starts_with('+') || l.starts_with('-'))
                && !l.starts_with("+++")
                && !l.starts_with("---")
        })
        .map(str::trim_end)
        .collect();
    (!lines.is_empty()).then(|| lines.join("\n"))
}

/// Match the patches an update adds (`ours`, and the commits its spec's
/// added `PatchN:` URLs name, `our_spec_shas`) against stock's
/// (`stock`, and the commits stock's spec names, `stock_spec_shas`).
/// `None` when the update adds nothing to compare, as a spec-only
/// change does.
pub fn compare(
    ours: &[PatchFile],
    our_spec_shas: &[String],
    stock: &[PatchFile],
    stock_spec_shas: &[String],
) -> Option<Comparison> {
    if ours.is_empty() && our_spec_shas.is_empty() {
        return None;
    }
    let mut c = Comparison::default();
    // A commit stock names anywhere: a patch's From line, or a spec URL.
    let stock_by_sha: Vec<(String, String)> = stock
        .iter()
        .flat_map(|p| {
            format_patch_shas(&p.text)
                .into_iter()
                .map(move |s| (s, p.name.clone()))
        })
        .chain(
            stock_spec_shas
                .iter()
                .map(|s| (s.clone(), "the spec's Patch URLs".to_string())),
        )
        .collect();
    let in_stock = |sha: &str| {
        stock_by_sha
            .iter()
            .find(|(s, _)| same_commit(s, sha))
            .map(|(_, name)| name.clone())
    };
    for p in ours {
        let shas = format_patch_shas(&p.text);
        let by_sha = (!shas.is_empty())
            .then(|| {
                shas.iter()
                    .map(|s| in_stock(s).map(|name| (s.clone(), name)))
                    .collect::<Option<Vec<_>>>()
            })
            .flatten();
        let by_change = || {
            let change = normalized_change(&p.text)?;
            stock
                .iter()
                .find(|s| normalized_change(&s.text).as_deref() == Some(&change))
                .map(|s| s.name.clone())
        };
        match by_sha {
            Some(found) => {
                let short: Vec<String> = found.iter().map(|(s, _)| short_sha(s)).collect();
                c.matched.push((
                    p.name.clone(),
                    found[0].1.clone(),
                    format!("upstream {}", short.join(", ")),
                ));
            }
            // No commit id, or one stock does not name: a cherry-pick
            // onto another branch gets a new id for the same change
            // (PackageKit's 22656f21 is stock's ac49526d), so the change
            // itself decides.
            None => match by_change() {
                Some(name) => {
                    let theirs = stock
                        .iter()
                        .find(|s| s.name == name)
                        .map(|s| format_patch_shas(&s.text))
                        .unwrap_or_default();
                    let how = match (shas.first(), theirs.first()) {
                        (Some(a), Some(b)) => format!(
                            "the same change, upstream {} here and {} in stock",
                            short_sha(a),
                            short_sha(b)
                        ),
                        _ => "the same change".to_string(),
                    };
                    c.matched.push((p.name.clone(), name, how));
                }
                None => c.missing.push(p.name.clone()),
            },
        }
    }
    for sha in our_spec_shas {
        match in_stock(sha) {
            Some(name) => c.matched.push((
                format!("Patch URL {}", &sha[..8.min(sha.len())]),
                name,
                format!("upstream {}", &sha[..8.min(sha.len())]),
            )),
            None => c.missing.push(format!("Patch URL {sha}")),
        }
    }
    Some(c)
}

/// A commit id as reports cite it.
fn short_sha(s: &str) -> String {
    s[..8.min(s.len())].to_string()
}

/// Whether two commit ids name the same commit: equal, or one a prefix
/// of the other of at least seven digits.
fn same_commit(a: &str, b: &str) -> bool {
    let (short, long) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    short.len() >= 7 && long.starts_with(short)
}

/// The URL on a spec's `PatchN:` or `Patch:` line, when it has one.
fn patch_line_url(line: &str) -> Option<&str> {
    let (tag, value) = line.trim().split_once(':')?;
    let tag = tag.trim();
    let is_patch = tag
        .strip_prefix("Patch")
        .is_some_and(|n| n.chars().all(|c| c.is_ascii_digit()));
    let value = value.trim();
    (is_patch && value.contains("://")).then_some(value)
}

fn is_hex(s: &str) -> bool {
    s.chars().all(|c| c.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    const AC49: &str = "ac49526d1c2b5e3f4a6b7c8d9e0f1a2b3c4d5e6f";

    fn format_patch(sha: &str, change: &str) -> String {
        format!(
            "From {sha} Mon Sep 17 00:00:00 2001\n\
             From: Matthias Klumpp <matthias@tenstral.net>\n\
             Subject: [PATCH] Do not allow re-invoking methods on non-new transactions\n\
             ---\n\
             diff --git a/src/pk-transaction.c b/src/pk-transaction.c\n\
             --- a/src/pk-transaction.c\n\
             +++ b/src/pk-transaction.c\n\
             @@ -10,6 +10,9 @@\n{change}"
        )
    }

    #[test]
    fn the_upstream_commit_is_read_from_format_patch_and_urls() {
        assert_eq!(format_patch_shas(&format_patch(AC49, "+x\n")), [AC49]);
        assert!(format_patch_shas("From: someone\nnot a header\n").is_empty());
        assert_eq!(
            url_sha("https://github.com/PackageKit/PackageKit/commit/ac49526d1c2b.patch")
                .as_deref(),
            Some("ac49526d1c2b")
        );
        assert_eq!(
            url_sha("https://git.kernel.org/pub/scm/x.git/patch/?id=0f599d36ab12").as_deref(),
            Some("0f599d36ab12")
        );
        assert_eq!(url_sha("https://example.com/fix.patch"), None);
        assert_eq!(
            spec_patch_shas(
                "Patch0: local.patch\n\
                 Patch12: https://github.com/a/b/commit/0f599d36ab12cd.patch\n\
                 Source0: https://github.com/a/b/commit/deadbeefdead\n"
            ),
            ["0f599d36ab12cd"]
        );
    }

    #[test]
    fn packagekit_s_fix_is_found_under_red_hat_s_own_file_name() {
        // The SIG's PK_… patch and stock's 0001-… carry the same commit.
        let ours = [PatchFile {
            name: "PK_Do-not-allow-re-invoking-methods-on-non-new-txn.patch".into(),
            text: format_patch(AC49, "+    if (state != NEW) return;\n"),
        }];
        let stock = [
            PatchFile {
                name: "runtime-warnings.patch".into(),
                text: "--- a\n+++ b\n+other\n".into(),
            },
            PatchFile {
                name: "0001-Do-not-allow-re-invoking-methods-on-non-new-transact.patch".into(),
                text: format_patch(AC49, "+    if (state != NEW)\n+        return;\n"),
            },
        ];
        let c = compare(&ours, &[], &stock, &[]).unwrap();
        assert!(c.all_carried(), "{c:?}");
        assert_eq!(
            c.matched[0].1,
            "0001-Do-not-allow-re-invoking-methods-on-non-new-transact.patch"
        );
        assert_eq!(c.matched[0].2, "upstream ac49526d");
    }

    #[test]
    fn a_cherry_pick_with_a_new_commit_id_is_matched_by_its_change() {
        // The SIG exported the fix from 22656f21, a cherry-pick of
        // stock's ac49526d; stock's file also carries format-patch's
        // signature.
        let change = "+    if (state != NEW)\n+        return;\n";
        let ours = [PatchFile {
            name: "PK_Do-not-allow-re-invoking-methods-on-non-new-txn.patch".into(),
            text: format_patch(&format!("22656f21{}", "0".repeat(32)), change),
        }];
        let stock = [PatchFile {
            name: "0001-Do-not-allow-re-invoking-methods-on-non-new-transact.patch".into(),
            text: format!("{}-- \n2.53.0\n", format_patch(AC49, change)),
        }];
        let c = compare(&ours, &[], &stock, &[]).unwrap();
        assert!(c.all_carried(), "{c:?}");
        assert_eq!(
            c.matched[0].2,
            "the same change, upstream 22656f21 here and ac49526d in stock"
        );
    }

    #[test]
    fn a_patch_stock_does_not_carry_is_missing_whatever_else_matches() {
        let ours = [
            PatchFile {
                name: "fix-a.patch".into(),
                text: format_patch(AC49, "+a\n"),
            },
            PatchFile {
                name: "fix-b.patch".into(),
                text: format_patch(&"b".repeat(40), "+b\n"),
            },
        ];
        let stock = [PatchFile {
            name: "0001-a.patch".into(),
            text: format_patch(AC49, "+a\n"),
        }];
        let c = compare(&ours, &[], &stock, &[]).unwrap();
        assert!(!c.all_carried());
        assert_eq!(c.missing, ["fix-b.patch"]);
    }

    #[test]
    fn without_a_commit_id_the_change_itself_is_compared() {
        let ours = [PatchFile {
            name: "local.patch".into(),
            text: "--- a/x\n+++ b/x\n@@ -1 +1 @@\n-old\n+new   \n".into(),
        }];
        // Renamed, re-exported at another position, trailing space gone.
        let stock = [PatchFile {
            name: "x-fix.patch".into(),
            text: "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -40 +40 @@\n-old\n+new\n".into(),
        }];
        let c = compare(&ours, &[], &stock, &[]).unwrap();
        assert_eq!(c.matched[0].2, "the same change");
        // A spec-only change adds nothing to compare.
        assert_eq!(compare(&[], &[], &stock, &[]), None);
    }

    #[test]
    fn a_spec_patch_url_matches_stock_s_patch_or_url() {
        let c = compare(
            &[],
            &["0f599d36ab12".into()],
            &[],
            &["0f599d36ab12cdef".into()],
        )
        .unwrap();
        assert!(c.all_carried(), "{c:?}");
        let c = compare(&[], &["0f599d36ab12".into()], &[], &[]).unwrap();
        assert_eq!(c.missing, ["Patch URL 0f599d36ab12"]);
    }

    #[test]
    fn a_merge_request_s_new_patches_and_patch_urls_are_read() {
        let diffs: Vec<sandogasa_gitlab::MergeRequestDiff> = serde_json::from_value(serde_json::json!([
            {"old_path": "PK_x.patch", "new_path": "PK_x.patch", "new_file": true,
             "diff": format!("@@ -0,0 +1,2 @@\n+From {AC49} Mon Sep 17 00:00:00 2001\n+Subject: x\n")},
            {"old_path": "PackageKit.spec", "new_path": "PackageKit.spec",
             "diff": "@@ -5,0 +6 @@\n+Patch20: https://github.com/a/b/commit/0f599d36ab12.patch\n Patch1: old.patch\n"},
        ]))
        .unwrap();
        let added = added_patches(&diffs);
        assert_eq!(added.len(), 1);
        assert_eq!(format_patch_shas(&added[0].text), [AC49]);
        assert_eq!(added_spec_shas(&diffs), ["0f599d36ab12"]);
    }
}
