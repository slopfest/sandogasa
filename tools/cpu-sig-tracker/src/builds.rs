// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A tracking issue's **Affected build** and **Expected fix** lines,
//! filled from the evidence the tool reads when nobody named them
//! (#101): the stock build the update is for, and the stock build
//! expected to carry the fix, which the SIG's own build names.

/// What an unknown build line says.
pub const UNKNOWN: &str = "_(unknown)_";

/// The two build lines, as `(prefix, value)`.
pub const AFFECTED: &str = "- **Affected build**:";
pub const EXPECTED: &str = "- **Expected fix**:";

/// The value of `body`'s line starting with `prefix`, if it has one
/// that is not [`UNKNOWN`].
pub fn known(body: &str, prefix: &str) -> Option<String> {
    body.lines().find_map(|l| {
        let v = l.trim_start().strip_prefix(prefix)?.trim();
        (!v.is_empty() && v != UNKNOWN).then(|| v.to_string())
    })
}

/// The SIG's build of the fix: the one in `-release`, else the one in
/// `-testing`.
pub fn sig_build(released: Option<&str>, testing: Option<&str>) -> Option<String> {
    released.or(testing).map(str::to_string)
}

/// The stock build expected to carry the fix once the merge request
/// lands and Red Hat builds it: the SIG's build without its
/// `~proposed`, as the SIG builds stock's next release with that suffix
/// so its build sorts just below stock's (`rsync-3.5.0-2~proposed.el10`
/// for `rsync-3.5.0-2.el10`). `None` for a build without the suffix.
pub fn expected_fix(sig_build: &str) -> Option<String> {
    sig_build
        .contains("~proposed")
        .then(|| sig_build.replacen("~proposed", "", 1))
}

/// The stock build the update is for: stock's current build, but only
/// while stock does not carry the change (`landed`), since "affected"
/// means the build that was vulnerable, and only when it is not past
/// the SIG's build of the fix (`fix`).
pub fn affected(stock: Option<&str>, fix: Option<&str>, landed: bool) -> Option<String> {
    if landed {
        return None;
    }
    let stock = stock?;
    if crate::status::stream_newer_than_proposed(fix, Some(stock)) {
        return None;
    }
    Some(stock.to_string())
}

/// `body` with its unknown (or missing) build lines filled: `affected`
/// and `expected` replace a line that says [`UNKNOWN`] or is absent,
/// and `expected` an expected fix naming a `~proposed` build, which is
/// the SIG's and never stock's; any other value is kept.
pub fn fill(body: &str, affected: Option<&str>, expected: Option<&str>) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut seen = (false, false);
    for line in body.lines() {
        let t = line.trim_start();
        let replaced = if t.starts_with(AFFECTED) {
            seen.0 = true;
            affected
                .filter(|_| known(t, AFFECTED).is_none())
                .map(|a| format!("{AFFECTED} {a}"))
        } else if t.starts_with(EXPECTED) {
            seen.1 = true;
            // A `~proposed` build is the SIG's own, never stock's, so an
            // expected fix naming one is a mistake to correct.
            let settled = known(t, EXPECTED).filter(|v| !v.contains("~proposed"));
            expected
                .filter(|_| settled.is_none())
                .map(|e| format!("{EXPECTED} {e}"))
        } else {
            None
        };
        out.push(replaced.unwrap_or_else(|| line.to_string()));
    }
    if let (false, Some(a)) = (seen.0, affected) {
        out.push(format!("{AFFECTED} {a}"));
    }
    if let (false, Some(e)) = (seen.1, expected) {
        out.push(format!("{EXPECTED} {e}"));
    }
    let mut s = out.join("\n");
    if body.ends_with('\n') {
        s.push('\n');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    /// rsync#1 as filed: both lines unknown.
    const FILED: &str = "- **MR**: [t](https://x/-/merge_requests/90) — opened\n\
                         - **JIRA**: [RHEL-303721](https://y) — New\n\
                         - **Release**: c10s\n\
                         - **Affected build**: _(unknown)_\n\
                         - **Expected fix**: _(unknown)_\n";

    #[test]
    fn unknown_build_lines_are_filled_and_known_ones_kept() {
        let out = fill(
            FILED,
            Some("rsync-3.5.0-1.el10"),
            Some("rsync-3.5.0-2.el10"),
        );
        assert!(
            out.contains("- **Affected build**: rsync-3.5.0-1.el10\n"),
            "{out}"
        );
        assert!(
            out.contains("- **Expected fix**: rsync-3.5.0-2.el10\n"),
            "{out}"
        );
        assert_eq!(known(&out, AFFECTED).as_deref(), Some("rsync-3.5.0-1.el10"));
        // Filled once, the values stay whatever comes later.
        assert_eq!(fill(&out, Some("rsync-9-9.el10"), Some("other")), out);
        // Nothing to fill with: unchanged.
        assert_eq!(fill(FILED, None, None), FILED);
        assert_eq!(known(FILED, EXPECTED), None);
    }

    /// rsync#1 after a refresh that named the SIG's build as the fix.
    #[test]
    fn an_expected_fix_naming_the_sigs_build_is_corrected() {
        let wrong = FILED.replace(
            "- **Expected fix**: _(unknown)_",
            "- **Expected fix**: rsync-3.5.0-2~proposed.el10",
        );
        let out = fill(&wrong, None, Some("rsync-3.5.0-2.el10"));
        assert!(
            out.contains("- **Expected fix**: rsync-3.5.0-2.el10\n"),
            "{out}"
        );
        assert!(!out.contains("~proposed"), "{out}");
    }

    #[test]
    fn a_body_without_the_lines_gains_them() {
        let out = fill("- **Release**: c9s\n", Some("a-1-1.el9"), None);
        assert_eq!(out, "- **Release**: c9s\n- **Affected build**: a-1-1.el9\n");
    }

    #[test]
    fn the_expected_fix_is_stocks_build_of_the_sigs_release() {
        let sig = sig_build(Some("rsync-3.5.0-2~proposed.el10"), Some("x")).unwrap();
        assert_eq!(sig, "rsync-3.5.0-2~proposed.el10");
        assert_eq!(sig_build(None, Some("t")).as_deref(), Some("t"));
        assert_eq!(sig_build(None, None), None);
        assert_eq!(expected_fix(&sig).as_deref(), Some("rsync-3.5.0-2.el10"));
        assert_eq!(
            expected_fix("rsync-3.2.7-2~proposed.el9").as_deref(),
            Some("rsync-3.2.7-2.el9")
        );
        // Not built the SIG's way: nothing to say of stock's.
        assert_eq!(expected_fix("rsync-3.5.0-2.el10"), None);
    }

    #[test]
    fn stock_is_the_affected_build_only_while_it_lacks_the_fix() {
        let fix = Some("rsync-3.5.0-2~proposed.el10");
        assert_eq!(
            affected(Some("rsync-3.5.0-1.el10"), fix, false).as_deref(),
            Some("rsync-3.5.0-1.el10")
        );
        // Stock carries the change: its build was never the affected one.
        assert_eq!(affected(Some("rsync-3.5.0-1.el10"), fix, true), None);
        // Stock is past the SIG's build: probably fixed, so not a guess
        // worth writing.
        assert_eq!(affected(Some("rsync-3.5.0-4.el10"), fix, false), None);
        assert_eq!(affected(None, fix, false), None);
    }
}
