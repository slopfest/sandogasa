// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Rewriting `debian/control`'s source stanza.
//!
//! Adopting upstream's `debian/` makes their packaging yours, and two
//! fields then point at the wrong people and the wrong place:
//! `Maintainer` names whoever wrote it, and `Vcs-Git`/`Vcs-Browser`
//! name their repository or nothing at all. A wrong `Vcs-*` is the
//! more costly of the two — it sends `debcheckout` and every future
//! contributor to a repository the packaging does not live in.
//!
//! Only the source stanza is touched. `Maintainer` and `Vcs-*` are
//! source fields; a binary stanza carrying something by that name is
//! not ours to rewrite.

/// Set `field` in the first (source) stanza, replacing any value it
/// already has, continuation lines included. `None` when the text has
/// no source stanza to edit — an empty file, or one that opens with a
/// comment and nothing else.
///
/// Appends to the end of the source stanza when the field is absent,
/// which is where `Vcs-*` conventionally sits and is in any case
/// valid: a stanza's fields are unordered.
pub fn set_source_field(control: &str, field: &str, value: &str) -> Option<String> {
    let end = source_stanza_end(control)?;
    let mut out: Vec<String> = Vec::new();
    let mut replaced = false;
    let mut skipping = false;
    for (i, line) in control.lines().enumerate() {
        if i >= end {
            if !replaced {
                out.push(format!("{field}: {value}"));
                replaced = true;
            }
            out.push(line.to_string());
            continue;
        }
        // A continuation line belongs to the field above it.
        if skipping {
            if line.starts_with(' ') || line.starts_with('\t') {
                continue;
            }
            skipping = false;
        }
        if field_name(line).is_some_and(|f| f.eq_ignore_ascii_case(field)) {
            out.push(format!("{field}: {value}"));
            replaced = true;
            skipping = true;
            continue;
        }
        out.push(line.to_string());
    }
    if !replaced {
        out.push(format!("{field}: {value}"));
    }
    let mut text = out.join("\n");
    if control.ends_with('\n') {
        text.push('\n');
    }
    Some(text)
}

/// The field a line names, if it names one. A continuation line and a
/// comment name nothing.
fn field_name(line: &str) -> Option<&str> {
    if line.starts_with(' ') || line.starts_with('\t') || line.starts_with('#') {
        return None;
    }
    line.split_once(':').map(|(name, _)| name.trim())
}

/// The line index the source stanza ends at — the first blank line, or
/// the end of the text. `None` when no field was seen before it, since
/// there is then no stanza to edit.
fn source_stanza_end(control: &str) -> Option<usize> {
    let mut seen_field = false;
    for (i, line) in control.lines().enumerate() {
        if line.trim().is_empty() {
            return seen_field.then_some(i);
        }
        if field_name(line).is_some() {
            seen_field = true;
        }
    }
    seen_field.then(|| control.lines().count())
}

/// The packager, as `debian/changelog` and `dch` would write them:
/// `DEBFULLNAME <DEBEMAIL>`. `None` when either is unset, because
/// guessing a maintainer from git config would put a name in a
/// published package that the packager never chose for it.
pub fn maintainer_from_env() -> Option<String> {
    let name = std::env::var("DEBFULLNAME").ok()?;
    let email = std::env::var("DEBEMAIL").ok()?;
    let (name, email) = (name.trim(), email.trim());
    (!name.is_empty() && !email.is_empty()).then(|| format!("{name} <{email}>"))
}

/// `(Vcs-Git, Vcs-Browser)` for a project's web URL.
///
/// Both are https: `Vcs-Git` is the URL anyone can clone from, not the
/// one the maintainer pushes to, so an ssh remote is converted rather
/// than copied.
pub fn vcs_fields(web_url: &str) -> (String, String) {
    let web = web_url.trim_end_matches('/').trim_end_matches(".git");
    (format!("{web}.git"), web.to_string())
}

/// The browsable https URL a git remote points at, for deriving
/// `Vcs-*` from the remote the packaging is actually pushed to.
/// `None` for anything that is not an ssh or https git URL.
pub fn web_url_from_remote(remote: &str) -> Option<String> {
    let remote = remote.trim().trim_end_matches('/');
    let rest = if let Some(rest) = remote.strip_prefix("https://") {
        rest.to_string()
    } else if let Some(rest) = remote.strip_prefix("ssh://") {
        // ssh://git@host[:port]/path
        let rest = rest.split_once('@').map_or(rest, |(_, r)| r);
        let (host, path) = rest.split_once('/')?;
        format!("{}/{path}", host.split_once(':').map_or(host, |(h, _)| h))
    } else if let Some((user_host, path)) = remote.split_once(':') {
        // git@host:namespace/name.git
        let host = user_host.split_once('@').map_or(user_host, |(_, h)| h);
        if host.is_empty() || path.is_empty() {
            return None;
        }
        format!("{host}/{path}")
    } else {
        return None;
    };
    let rest = rest.trim_end_matches(".git");
    (!rest.is_empty() && rest.contains('/')).then(|| format!("https://{rest}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONTROL: &str = "\
Source: antifennel
Section: devel
Priority: optional
Maintainer: Phil Hagelberg <phil@hagelb.org>
Build-Depends: debhelper-compat (= 13),
               fennel
Standards-Version: 4.6.2
Homepage: https://git.sr.ht/~technomancy/antifennel

Package: antifennel
Architecture: all
Depends: ${misc:Depends}
Description: compile Lua back to Fennel
 Longer description.
";

    /// The field is replaced where it stands, and the rest of the file
    /// is untouched -- including the binary stanza, which is not ours
    /// to rewrite even when it carries a field of the same name.
    #[test]
    fn a_source_field_is_replaced_in_place() {
        let out = set_source_field(
            CONTROL,
            "Maintainer",
            "Michel Lind <michel@michel-slm.name>",
        )
        .expect("a source stanza");
        assert!(out.contains("Maintainer: Michel Lind <michel@michel-slm.name>\n"));
        assert!(!out.contains("Phil Hagelberg"));
        // Everything else survives, in order.
        assert!(out.contains("Source: antifennel\nSection: devel\n"));
        assert!(out.contains("Package: antifennel\nArchitecture: all\n"));
        assert!(out.ends_with(" Longer description.\n"));
        assert_eq!(out.lines().count(), CONTROL.lines().count());
    }

    /// A field spanning continuation lines is replaced whole, rather
    /// than leaving its continuations orphaned under the new value.
    #[test]
    fn a_continued_field_is_replaced_whole() {
        let out = set_source_field(CONTROL, "Build-Depends", "debhelper-compat (= 13)")
            .expect("a source stanza");
        assert!(out.contains("Build-Depends: debhelper-compat (= 13)\nStandards-Version:"));
        assert!(!out.contains("               fennel"));
    }

    /// An absent field is added at the end of the source stanza, not
    /// after the binary one -- which is what `Vcs-*` needs, since
    /// upstream's packaging usually has neither.
    #[test]
    fn an_absent_field_lands_in_the_source_stanza() {
        let out = set_source_field(CONTROL, "Vcs-Git", "https://salsa.debian.org/michel/x.git")
            .expect("a source stanza");
        let vcs = out.find("Vcs-Git").expect("added");
        let binary = out.find("Package: antifennel").expect("binary stanza");
        assert!(vcs < binary, "it must not land in the binary stanza");
        assert!(out.contains("Homepage: https://git.sr.ht/~technomancy/antifennel\nVcs-Git:"));
    }

    /// Nothing to edit is said with None rather than by inventing a
    /// stanza: `edit_file` then leaves the file alone and warns.
    #[test]
    fn a_file_with_no_stanza_is_refused() {
        assert!(set_source_field("", "Maintainer", "x").is_none());
        assert!(set_source_field("# just a comment\n", "Maintainer", "x").is_none());
    }

    /// The three fields the adoption step sets, applied as it applies
    /// them: Maintainer replaced where it stands, both Vcs-* appended
    /// to the source stanza, binary stanza untouched.
    #[test]
    fn claiming_packaging_sets_all_three_fields() {
        let (vcs_git, vcs_browser) = vcs_fields(
            &web_url_from_remote("git@salsa.debian.org:michel/antifennel.git").expect("a url"),
        );
        let out = set_source_field(
            CONTROL,
            "Maintainer",
            "Michel Lind <michel@michel-slm.name>",
        )
        .and_then(|t| set_source_field(&t, "Vcs-Git", &vcs_git))
        .and_then(|t| set_source_field(&t, "Vcs-Browser", &vcs_browser))
        .expect("a source stanza");

        assert_eq!(
            out,
            "\
Source: antifennel
Section: devel
Priority: optional
Maintainer: Michel Lind <michel@michel-slm.name>
Build-Depends: debhelper-compat (= 13),
               fennel
Standards-Version: 4.6.2
Homepage: https://git.sr.ht/~technomancy/antifennel
Vcs-Git: https://salsa.debian.org/michel/antifennel.git
Vcs-Browser: https://salsa.debian.org/michel/antifennel

Package: antifennel
Architecture: all
Depends: ${misc:Depends}
Description: compile Lua back to Fennel
 Longer description.
"
        );
    }

    #[test]
    fn vcs_fields_are_both_https_and_consistent() {
        let (git, browser) = vcs_fields("https://salsa.debian.org/michel/antifennel");
        assert_eq!(git, "https://salsa.debian.org/michel/antifennel.git");
        assert_eq!(browser, "https://salsa.debian.org/michel/antifennel");
        // A trailing slash or .git on the input changes nothing.
        assert_eq!(
            vcs_fields("https://salsa.debian.org/michel/antifennel.git").0,
            git
        );
    }

    /// The push remote is usually ssh, and Vcs-Git is the URL a
    /// stranger clones from, so it is converted rather than copied.
    #[test]
    fn a_remote_becomes_a_browsable_url() {
        for remote in [
            "git@salsa.debian.org:michel/antifennel.git",
            "ssh://git@salsa.debian.org/michel/antifennel.git",
            "https://salsa.debian.org/michel/antifennel.git",
            "https://salsa.debian.org/michel/antifennel/",
        ] {
            assert_eq!(
                web_url_from_remote(remote).as_deref(),
                Some("https://salsa.debian.org/michel/antifennel"),
                "{remote}"
            );
        }
        assert_eq!(web_url_from_remote("/srv/git/antifennel"), None);
        assert_eq!(web_url_from_remote(""), None);
    }
}
