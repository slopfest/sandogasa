// SPDX-License-Identifier: Apache-2.0 OR MIT

//! RPM dependency strings: rich (boolean) dependencies evaluated with
//! their semantics, capability names and versions taken out of them.

use std::collections::BTreeSet;

/// A plain dependency's capability and constraint: `foo >= 1.2` gives
/// `("foo", Some((">=", "1.2")))`, `foo` gives `("foo", None)`; `None`
/// for a rich dependency or anything else.
pub fn parse_versioned_dep(dep: &str) -> Option<(&str, Option<(&str, &str)>)> {
    let dep = dep.trim();
    if dep.starts_with('(') {
        return None;
    }
    let parts: Vec<&str> = dep.split_whitespace().collect();
    match parts.as_slice() {
        [cap] => Some((cap, None)),
        [cap, op, ver] if matches!(*op, "<" | ">" | "<=" | ">=" | "=") => {
            Some((cap, Some((op, ver))))
        }
        _ => None,
    }
}

/// Strip "grouping" parens from a token's ends while keeping parens
/// that belong to a capability name. A trailing `)` is dropped only
/// when the token has more `)` than `(` (so it closes an outer rich-dep
/// group), and likewise a leading `(`; thus `sound-theme-freedesktop)`
/// → `sound-theme-freedesktop`, but `crate(foo)` and
/// `libc.so.6(GLIBC_2.34)(64bit)` are left intact.
fn strip_grouping_parens(token: &str) -> &str {
    let mut s = token;
    loop {
        let opens = s.matches('(').count();
        let closes = s.matches(')').count();
        if closes > opens && s.ends_with(')') {
            s = &s[..s.len() - 1];
        } else if opens > closes && s.starts_with('(') {
            s = &s[1..];
        } else {
            break;
        }
    }
    s
}

/// The bare capability name of a leaf term: its first whitespace token
/// (dropping any version constraint like `foo >= 1.0`) with grouping
/// parens stripped.
pub fn leaf_cap(term: &str) -> &str {
    strip_grouping_parens(term.split_whitespace().next().unwrap_or(""))
}

/// Byte range of the boolean operator `op` (a standalone whitespace-
/// delimited token) at paren depth 0 in `expr`, if any.
fn find_top_op(expr: &str, op: &str) -> Option<(usize, usize)> {
    let bytes = expr.as_bytes();
    let mut depth = 0i32;
    for (idx, &c) in bytes.iter().enumerate() {
        match c {
            b'(' => depth += 1,
            b')' => depth -= 1,
            b' ' if depth == 0 => {
                let ws = idx + 1;
                let we = expr[ws..].find(' ').map(|p| ws + p).unwrap_or(expr.len());
                if &expr[ws..we] == op {
                    return Some((ws, we));
                }
            }
            _ => {}
        }
    }
    None
}

/// Split `expr` on the first top-level `op` token into `(before, after)`.
fn split_top<'a>(expr: &'a str, op: &str) -> Option<(&'a str, &'a str)> {
    find_top_op(expr, op).map(|(s, e)| (expr[..s].trim(), expr[e..].trim()))
}

/// Split `expr` on every top-level `op` token (≥2 parts means the
/// operator is present at the top level).
fn split_all_top<'a>(expr: &'a str, op: &str) -> Vec<&'a str> {
    let mut parts = Vec::new();
    let mut rest = expr;
    while let Some((before, after)) = split_top(rest, op) {
        parts.push(before);
        rest = after;
    }
    parts.push(rest.trim());
    parts
}

/// Whether an RPM dependency is satisfiable, given `resolves(cap)` for
/// a bare capability name. Evaluates boolean/rich deps with their real
/// semantics (a depsolver-free approximation): `A if B` requires A only
/// when B resolves; `A unless B` requires A only when B does *not*;
/// `or` needs any term; `and`/`with` need all; `without` ignores the
/// excluded term. A plain term is satisfiable when its capability
/// resolves.
pub fn dep_satisfiable(dep: &str, resolves: &impl Fn(&str) -> bool) -> bool {
    let dep = dep.trim();
    match dep.strip_prefix('(').and_then(|s| s.strip_suffix(')')) {
        // Boolean/rich dep (fully parenthesized): evaluate the inside.
        Some(inner) => eval_boolean(inner.trim(), resolves),
        // Leaf terms reach the resolver whole — version constraint
        // included — so version-aware resolvers can judge them;
        // name-level resolvers strip the constraint themselves
        // (see `leaf_cap`).
        None => dep.is_empty() || resolves(dep),
    }
}

fn eval_boolean(expr: &str, resolves: &impl Fn(&str) -> bool) -> bool {
    // `A if B [else C]` — conditional.
    if let Some((cons, rest)) = split_top(expr, "if") {
        if let Some((cond, alt)) = split_top(rest, "else") {
            return if eval_boolean(cond, resolves) {
                eval_boolean(cons, resolves)
            } else {
                eval_boolean(alt, resolves)
            };
        }
        // No `else`: the requirement applies only when the condition holds.
        return !eval_boolean(rest, resolves) || eval_boolean(cons, resolves);
    }
    // `A unless B` — inverse conditional.
    if let Some((cons, cond)) = split_top(expr, "unless") {
        return eval_boolean(cond, resolves) || eval_boolean(cons, resolves);
    }
    // `A or B …` — any term.
    let ors = split_all_top(expr, "or");
    if ors.len() > 1 {
        return ors.iter().any(|t| eval_boolean(t, resolves));
    }
    // `A and B …` / `A with B …` — all terms.
    for op in ["and", "with"] {
        let terms = split_all_top(expr, op);
        if terms.len() > 1 {
            return terms.iter().all(|t| eval_boolean(t, resolves));
        }
    }
    // `A without B` — only A matters.
    if let Some((a, _b)) = split_top(expr, "without") {
        return eval_boolean(a, resolves);
    }
    // Nested group or a plain leaf (passed to the resolver whole,
    // version constraint included).
    let expr = expr.trim();
    match expr.strip_prefix('(').and_then(|s| s.strip_suffix(')')) {
        Some(inner) => eval_boolean(inner.trim(), resolves),
        None => expr.is_empty() || resolves(expr),
    }
}

/// Extract the bare capability names referenced by a dependency string
/// (for reporting which parts of a flagged dep didn't resolve). Strips
/// version constraints, boolean operators, and grouping parens — while
/// keeping parens that are part of a name (`crate(foo)`,
/// `libc.so.6(GLIBC_2.34)(64bit)`).
pub fn extract_capability_names(dep: &str) -> Vec<String> {
    let dep = dep.trim();
    let dep = match dep.strip_prefix('(').and_then(|s| s.strip_suffix(')')) {
        Some(inner) => inner,
        None => dep,
    };
    dep.split_whitespace()
        .filter(|token| {
            !matches!(
                *token,
                ">=" | "<=" | ">" | "<" | "=" | "with" | "or" | "and" | "if" | "unless" | "else"
            ) && !token.starts_with(|c: char| c.is_ascii_digit())
                && !token.ends_with('~')
        })
        .map(|s| strip_grouping_parens(s).to_string())
        .filter(|s| !s.is_empty())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// The version part of a Provide string, if any
/// (`crate(ctor/default) = 1.0.8` → `1.0.8`).
pub fn provide_version(provide: &str) -> Option<&str> {
    for op in [" = ", " >= ", " <= ", " < ", " > "] {
        if let Some((_, version)) = provide.split_once(op) {
            return Some(version.trim());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dep_satisfiable_if_condition_present_checks_consequent() {
        // The plasma-settings case: pulseaudio IS available, so the
        // consequent (both modules) is required — and both are present.
        let dep = "((pulseaudio-module-gsettings and sound-theme-freedesktop) if pulseaudio)";
        let all = |_: &str| true;
        assert!(dep_satisfiable(dep, &all));
        // pulseaudio present, but a consequent capability missing → broken.
        let missing_module = |c: &str| c != "sound-theme-freedesktop";
        assert!(!dep_satisfiable(dep, &missing_module));
        // pulseaudio absent → the requirement doesn't apply → satisfied,
        // even though the modules aren't available.
        let only_modules_absent = |c: &str| c == "irrelevant";
        assert!(dep_satisfiable(dep, &only_modules_absent));
    }

    #[test]
    fn dep_satisfiable_or_needs_any() {
        let dep = "(foo or bar)";
        assert!(dep_satisfiable(dep, &|c: &str| c == "bar")); // one is enough
        assert!(!dep_satisfiable(dep, &|_: &str| false)); // neither
    }

    #[test]
    fn dep_satisfiable_and_needs_all() {
        let dep = "(foo and bar)";
        assert!(dep_satisfiable(dep, &|_: &str| true));
        assert!(!dep_satisfiable(dep, &|c: &str| c == "foo")); // bar missing
    }

    #[test]
    fn dep_satisfiable_unless_waives_when_condition_present() {
        let dep = "(foo unless bar)";
        // bar present → foo not required → satisfied even if foo absent.
        assert!(dep_satisfiable(dep, &|c: &str| c == "bar"));
        // bar absent → foo required.
        assert!(!dep_satisfiable(dep, &|_: &str| false));
        assert!(dep_satisfiable(dep, &|c: &str| c == "foo"));
    }

    #[test]
    fn provide_version_extraction() {
        assert_eq!(
            provide_version("crate(ctor/default) = 1.0.8"),
            Some("1.0.8")
        );
        assert_eq!(
            provide_version("tor = 0.4.9.5-1.el9"),
            Some("0.4.9.5-1.el9")
        );
        assert_eq!(provide_version("libthing.so.1()(64bit)"), None);
    }

    #[test]
    fn dep_satisfiable_plain_dep_checks_capability() {
        // The resolver receives the whole leaf term, version
        // constraint included; name-level resolvers strip it with
        // leaf_cap (as check_update_installability does).
        assert!(dep_satisfiable("bash >= 5.0", &|t: &str| leaf_cap(t) == "bash"));
        assert!(!dep_satisfiable("missing-cap", &|_: &str| false));
        // A plain lib cap with parens in the name resolves as one cap.
        assert!(dep_satisfiable(
            "libc.so.6(GLIBC_2.34)(64bit)",
            &|c: &str| c == "libc.so.6(GLIBC_2.34)(64bit)"
        ));
    }

    #[test]
    fn extract_capability_names_strips_inner_group_close_paren() {
        // Regression: the close paren of an inner rich-dep group must
        // not stick to the capability name (the plasma-settings bug —
        // `sound-theme-freedesktop)` never resolved).
        let caps = extract_capability_names(
            "((pulseaudio-module-gsettings and sound-theme-freedesktop) if pulseaudio)",
        );
        assert!(caps.contains(&"pulseaudio-module-gsettings".to_string()));
        assert!(caps.contains(&"sound-theme-freedesktop".to_string()));
        assert!(caps.contains(&"pulseaudio".to_string()));
        assert!(!caps.iter().any(|c| c.ends_with(')') && !c.contains('(')));
    }

    #[test]
    fn extract_capability_names_keeps_paren_in_lib_caps() {
        // The trailing ')' on these caps is part of the name —
        // dropping it produces a corrupted cap that fedrq can't
        // resolve, surfacing as a false-positive installability
        // issue for every system library.
        assert_eq!(
            extract_capability_names("libc.so.6(GLIBC_2.34)(64bit)"),
            vec!["libc.so.6(GLIBC_2.34)(64bit)".to_string()]
        );
        assert_eq!(
            extract_capability_names("ld-linux-aarch64.so.1()(64bit)"),
            vec!["ld-linux-aarch64.so.1()(64bit)".to_string()]
        );
        assert_eq!(
            extract_capability_names("rtld(GNU_HASH)"),
            vec!["rtld(GNU_HASH)".to_string()]
        );
        assert_eq!(
            extract_capability_names("nginx(abi)"),
            vec!["nginx(abi)".to_string()]
        );
    }

    #[test]
    fn extract_capability_names_strips_outer_rich_dep_parens() {
        // A true rich/boolean dep wrapped in `(...)` should have
        // those wrapping parens stripped, but the inner cap
        // names with their own parens stay intact.
        let caps = extract_capability_names("(crate(foo) >= 1.0 with crate(foo) < 2.0~)");
        assert!(caps.contains(&"crate(foo)".to_string()));
        assert!(!caps.iter().any(|c| c.contains(">=") || c.contains("<")));
    }

    #[test]
    fn extract_capability_names_drops_version_operators() {
        let caps = extract_capability_names("nginx(abi) = 2:1.30.0-1.fc44");
        assert_eq!(caps, vec!["nginx(abi)".to_string()]);
    }
}
