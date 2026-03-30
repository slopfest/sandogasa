// SPDX-License-Identifier: MPL-2.0

//! RPM version comparison algorithm.
//!
//! Implements the same logic as `rpmvercmp()` in librpm, including
//! special handling for `~` (pre-release) and `^` (post-release
//! snapshot) characters.

use std::cmp::Ordering;

/// Compare two version strings using the RPM vercmp algorithm.
///
/// Key behaviours:
/// - `~` sorts *before* the version without it:
///   `1.0~rc1 < 1.0 < 1.0.1`
/// - `^` sorts *after* the base version but before a new segment:
///   `1.0 < 1.0^post1 < 1.0.1`
/// - Digit segments compare numerically (leading zeros ignored).
/// - Letter segments compare lexicographically.
/// - A digit segment is always newer than a letter segment.
/// - More segments means newer when all preceding segments are
///   equal.
pub fn rpmvercmp(a: &str, b: &str) -> Ordering {
    let mut a = a.as_bytes();
    let mut b = b.as_bytes();

    loop {
        // Skip non-alphanumeric characters that are not ~ or ^.
        a = skip_separators(a);
        b = skip_separators(b);

        // Handle ~ (pre-release): sorts before everything,
        // including end-of-string.
        match (a.first() == Some(&b'~'), b.first() == Some(&b'~')) {
            (true, true) => {
                a = &a[1..];
                b = &b[1..];
                continue;
            }
            (true, false) => return Ordering::Less,
            (false, true) => return Ordering::Greater,
            _ => {}
        }

        // Handle ^ (post-release snapshot): sorts after
        // end-of-string but before any other segment.
        match (a.first() == Some(&b'^'), b.first() == Some(&b'^')) {
            (true, true) => {
                a = &a[1..];
                b = &b[1..];
                continue;
            }
            (true, false) => {
                return if b.is_empty() {
                    Ordering::Greater
                } else {
                    Ordering::Less
                };
            }
            (false, true) => {
                return if a.is_empty() {
                    Ordering::Less
                } else {
                    Ordering::Greater
                };
            }
            _ => {}
        }

        // Both exhausted → equal.
        if a.is_empty() && b.is_empty() {
            return Ordering::Equal;
        }
        // One exhausted → the one with more segments is newer.
        if a.is_empty() {
            return Ordering::Less;
        }
        if b.is_empty() {
            return Ordering::Greater;
        }

        // Extract the next segment (run of digits or run of
        // letters).
        let (seg_a, rest_a) = next_segment(a);
        let (seg_b, rest_b) = next_segment(b);
        a = rest_a;
        b = rest_b;

        let is_a_num =
            seg_a.first().is_some_and(|c| c.is_ascii_digit());
        let is_b_num =
            seg_b.first().is_some_and(|c| c.is_ascii_digit());

        match (is_a_num, is_b_num) {
            // Digit segment always beats letter segment.
            (true, false) => return Ordering::Greater,
            (false, true) => return Ordering::Less,
            (true, true) => {
                // Compare numerically: strip leading zeros,
                // longer number is bigger; same length →
                // lexicographic.
                let at = trim_leading_zeros(seg_a);
                let bt = trim_leading_zeros(seg_b);
                match at.len().cmp(&bt.len()) {
                    Ordering::Equal => match at.cmp(bt) {
                        Ordering::Equal => continue,
                        ord => return ord,
                    },
                    ord => return ord,
                }
            }
            (false, false) => match seg_a.cmp(seg_b) {
                Ordering::Equal => continue,
                ord => return ord,
            },
        }
    }
}

/// Skip bytes that are not alphanumeric and not `~` or `^`.
fn skip_separators(s: &[u8]) -> &[u8] {
    let n = s
        .iter()
        .position(|c| {
            c.is_ascii_alphanumeric()
                || *c == b'~'
                || *c == b'^'
        })
        .unwrap_or(s.len());
    &s[n..]
}

/// Extract the next segment: a run of digits or a run of ASCII
/// letters. Returns (segment, rest).
fn next_segment(s: &[u8]) -> (&[u8], &[u8]) {
    if s.is_empty() {
        return (s, s);
    }
    let is_digit = s[0].is_ascii_digit();
    let len = s
        .iter()
        .position(|c| {
            if is_digit {
                !c.is_ascii_digit()
            } else {
                !c.is_ascii_alphabetic()
            }
        })
        .unwrap_or(s.len());
    (&s[..len], &s[len..])
}

fn trim_leading_zeros(s: &[u8]) -> &[u8] {
    let n = s
        .iter()
        .position(|c| *c != b'0')
        .unwrap_or(s.len());
    &s[n..]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_equal() {
        assert_eq!(rpmvercmp("1.0", "1.0"), Ordering::Equal);
    }

    #[test]
    fn test_numeric() {
        assert_eq!(rpmvercmp("1.1", "1.2"), Ordering::Less);
        assert_eq!(rpmvercmp("1.2", "1.1"), Ordering::Greater);
    }

    #[test]
    fn test_longer_numeric() {
        assert_eq!(rpmvercmp("1.10", "1.9"), Ordering::Greater);
        assert_eq!(rpmvercmp("1.9", "1.10"), Ordering::Less);
    }

    #[test]
    fn test_alpha_vs_numeric() {
        assert_eq!(rpmvercmp("1.0a", "1.01"), Ordering::Less);
        assert_eq!(rpmvercmp("1.01", "1.0a"), Ordering::Greater);
    }

    #[test]
    fn test_alpha() {
        assert_eq!(rpmvercmp("1.0a", "1.0b"), Ordering::Less);
        assert_eq!(rpmvercmp("1.0b", "1.0a"), Ordering::Greater);
    }

    #[test]
    fn test_more_segments() {
        assert_eq!(rpmvercmp("1.0.0", "1.0"), Ordering::Greater);
        assert_eq!(rpmvercmp("1.0", "1.0.0"), Ordering::Less);
    }

    #[test]
    fn test_leading_zeros() {
        assert_eq!(rpmvercmp("1.01", "1.1"), Ordering::Equal);
    }

    #[test]
    fn test_tilde_prerelease() {
        // ~ makes a version sort before the version without it
        assert_eq!(
            rpmvercmp("1.0~rc1", "1.0"),
            Ordering::Less
        );
        assert_eq!(
            rpmvercmp("1.0", "1.0~rc1"),
            Ordering::Greater
        );
    }

    #[test]
    fn test_tilde_both() {
        assert_eq!(
            rpmvercmp("1.0~rc1", "1.0~rc2"),
            Ordering::Less
        );
        assert_eq!(
            rpmvercmp("1.0~rc2", "1.0~rc1"),
            Ordering::Greater
        );
    }

    #[test]
    fn test_tilde_less_than_release() {
        // 6.19~rc6 < 6.19 < 6.19.6
        assert_eq!(
            rpmvercmp("6.19~rc6", "6.19"),
            Ordering::Less
        );
        assert_eq!(
            rpmvercmp("6.19~rc6", "6.19.6"),
            Ordering::Less
        );
        assert_eq!(
            rpmvercmp("6.19", "6.19.6"),
            Ordering::Less
        );
    }

    #[test]
    fn test_caret_postrelease() {
        // ^ makes a version sort after the base but before
        // a new segment
        assert_eq!(
            rpmvercmp("1.0^post1", "1.0"),
            Ordering::Greater
        );
        assert_eq!(
            rpmvercmp("1.0^post1", "1.0.1"),
            Ordering::Less
        );
    }

    #[test]
    fn test_caret_both() {
        assert_eq!(
            rpmvercmp("1.0^post1", "1.0^post2"),
            Ordering::Less
        );
    }

    #[test]
    fn test_tilde_before_caret() {
        assert_eq!(
            rpmvercmp("1.0~rc1", "1.0^post1"),
            Ordering::Less
        );
    }

    #[test]
    fn test_real_world_perf() {
        // The motivating case: upstream 6.19.6 should be
        // newer than Hyperscale 6.19~rc6
        assert_eq!(
            rpmvercmp("6.19.6", "6.19~rc6"),
            Ordering::Greater
        );
        assert_eq!(
            rpmvercmp("6.19~rc6", "6.19.6"),
            Ordering::Less
        );
    }

    #[test]
    fn test_empty() {
        assert_eq!(rpmvercmp("", ""), Ordering::Equal);
        assert_eq!(rpmvercmp("1.0", ""), Ordering::Greater);
        assert_eq!(rpmvercmp("", "1.0"), Ordering::Less);
    }

    #[test]
    fn test_kernel_versions() {
        assert_eq!(
            rpmvercmp("6.18.16", "6.18.3"),
            Ordering::Greater
        );
        assert_eq!(
            rpmvercmp("7.0.0", "5.7.9"),
            Ordering::Greater
        );
        assert_eq!(
            rpmvercmp("10.0", "9.0"),
            Ordering::Greater
        );
    }

    #[test]
    fn test_rc_comparison() {
        assert_eq!(
            rpmvercmp("7.0.0~rc2", "6.19.6"),
            Ordering::Greater
        );
    }
}
