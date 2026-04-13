// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Shell-style brace expansion for tag patterns.
//!
//! Expands patterns like `hyperscale{9,10}{,s}-packages-main-release`
//! into all combinations:
//! - `hyperscale9-packages-main-release`
//! - `hyperscale9s-packages-main-release`
//! - `hyperscale10-packages-main-release`
//! - `hyperscale10s-packages-main-release`

/// Expand all `{a,b,...}` groups in a pattern.
///
/// Supports multiple brace groups and nested-free expansion.
/// Returns a single-element vec if there are no braces.
pub fn expand(pattern: &str) -> Vec<String> {
    // Find the first `{...}` group.
    let Some(open) = pattern.find('{') else {
        return vec![pattern.to_string()];
    };
    let Some(close) = pattern[open..].find('}') else {
        return vec![pattern.to_string()];
    };
    let close = open + close;

    let prefix = &pattern[..open];
    let alternatives = &pattern[open + 1..close];
    let suffix = &pattern[close + 1..];

    // Split alternatives on comma and recurse on the suffix
    // (which may contain more brace groups).
    let mut results = Vec::new();
    for alt in alternatives.split(',') {
        let combined = format!("{prefix}{alt}{suffix}");
        results.extend(expand(&combined));
    }

    results
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_braces() {
        assert_eq!(expand("hello"), vec!["hello"]);
    }

    #[test]
    fn single_group() {
        let mut result = expand("foo{a,b,c}bar");
        result.sort();
        assert_eq!(result, vec!["fooabar", "foobbar", "foocbar"]);
    }

    #[test]
    fn empty_alternative() {
        let mut result = expand("test{,s}");
        result.sort();
        assert_eq!(result, vec!["test", "tests"]);
    }

    #[test]
    fn two_groups() {
        let mut result = expand("x{1,2}{a,b}");
        result.sort();
        assert_eq!(result, vec!["x1a", "x1b", "x2a", "x2b"]);
    }

    #[test]
    fn hyperscale_pattern() {
        let mut result = expand("hyperscale{9,10}{,s}-packages-main-release");
        result.sort();
        assert_eq!(
            result,
            vec![
                "hyperscale10-packages-main-release",
                "hyperscale10s-packages-main-release",
                "hyperscale9-packages-main-release",
                "hyperscale9s-packages-main-release",
            ]
        );
    }

    #[test]
    fn three_groups() {
        let mut result = expand("{a,b}{1,2}{x,y}");
        result.sort();
        assert_eq!(
            result,
            vec!["a1x", "a1y", "a2x", "a2y", "b1x", "b1y", "b2x", "b2y"]
        );
    }

    #[test]
    fn real_koji_pattern() {
        let mut result = expand("hyperscale{9,10}{,s}-packages-{main,facebook}-release");
        result.sort();
        assert_eq!(result.len(), 8);
        assert!(result.contains(&"hyperscale9-packages-main-release".to_string()));
        assert!(result.contains(&"hyperscale10s-packages-facebook-release".to_string()));
    }
}
