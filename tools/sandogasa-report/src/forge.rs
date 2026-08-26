// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Helpers shared by the forge backends (`gitlab`, `github`,
//! `forgejo`, `sourcehut`). Each backend keeps its own thin
//! `find_token` / `instance_token_env` wrappers naming its
//! [`TokenSpec`], so the env-var names stay byte-identical per
//! service; only the boilerplate lives here.

use std::collections::BTreeMap;

use chrono::NaiveDate;

/// Per-service parameters for the token lookup: everything that
/// differs between the four forges.
pub(crate) struct TokenSpec {
    /// Human-readable service name, used in the error message
    /// ("no GitLab token for …").
    pub service: &'static str,
    /// The generic env var, which is also the prefix of the
    /// instance-specific one (`<generic>_<HOST>`).
    pub generic_env: &'static str,
    /// Characters replaced by `_` when turning a hostname into an
    /// env-var suffix. Kept per-service because the names are a
    /// documented interface: Sourcehut also folds `-`, the others
    /// only `.`.
    pub host_separators: &'static [char],
    /// Extra text appended to the error message (e.g. where to
    /// generate a token). Empty for most services.
    pub hint: &'static str,
}

/// Whether an RFC 3339 timestamp's date falls within
/// `[since, until]` (inclusive). Only the date part is considered.
pub(crate) fn date_in_range(ts: &str, since: NaiveDate, until: NaiveDate) -> bool {
    let Some(day) = ts.split('T').next() else {
        return false;
    };
    NaiveDate::parse_from_str(day, "%Y-%m-%d")
        .map(|d| d >= since && d <= until)
        .unwrap_or(false)
}

/// Strip scheme + trailing slash to get the bare hostname — the
/// token-keying host. Note that an API base such as
/// `api.github.com` keeps its `api.` prefix, so a user with both
/// github.com and a GHES `api.example.com` ends up with two
/// distinct keys.
pub(crate) fn instance_host(instance: &str) -> String {
    instance
        .trim_end_matches('/')
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .to_string()
}

/// Name of the instance-specific token env var for `instance`,
/// e.g. `GITLAB_TOKEN_SALSA_DEBIAN_ORG`.
pub(crate) fn instance_token_env(instance: &str, spec: &TokenSpec) -> String {
    format!(
        "{}_{}",
        spec.generic_env,
        instance_host(instance)
            .to_uppercase()
            .replace(spec.host_separators, "_")
    )
}

/// Look up a forge API token for an instance.
///
/// Order: instance-specific env var → generic env var →
/// `<service>_tokens.<hostname>` from the user overlay → error.
/// Env vars win over config so a shell override works even with a
/// persisted token.
pub(crate) fn find_token(
    instance: &str,
    tokens: &BTreeMap<String, String>,
    spec: &TokenSpec,
) -> Result<String, String> {
    let var = instance_token_env(instance, spec);
    if let Ok(t) = std::env::var(&var) {
        return Ok(t);
    }
    if let Ok(t) = std::env::var(spec.generic_env) {
        return Ok(t);
    }
    let host = instance_host(instance);
    if let Some(t) = tokens.get(&host) {
        return Ok(t.clone());
    }
    Err(format!(
        "no {} token for {host}: set {var} (instance-specific), \
         {} (generic), or run `sandogasa-report config` to \
         store one in the overlay{}",
        spec.service, spec.generic_env, spec.hint
    ))
}

/// Append a `- **<label>:** <count>` summary bullet, unless there
/// is nothing to count.
///
/// A summary block exists to say what happened, so a line saying
/// something did not happen is noise — and there are enough of
/// them that a quiet period rendered as a wall of zeroes. The
/// backends already suppressed their `applied` counts this way;
/// this applies the same rule to every line.
pub fn stat(out: &mut String, label: &str, count: usize) {
    if count > 0 {
        out.push_str(&format!("- **{label}:** {count}\n"));
    }
}

/// Append a `- **<label>:** <total> across <n> <unit>(s)` bullet,
/// unless the total is zero. `unit` is the bare noun (`project`,
/// `repo`); the `(s)` is added here.
pub fn stat_across(out: &mut String, label: &str, total: u64, n: usize, unit: &str) {
    if total > 0 {
        out.push_str(&format!("- **{label}:** {total} across {n} {unit}(s)\n"));
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn stat_omits_a_zero_and_keeps_a_count() {
        let mut out = String::new();
        stat(&mut out, "PRs opened", 0);
        assert!(out.is_empty());
        stat(&mut out, "PRs merged", 3);
        assert_eq!(out, "- **PRs merged:** 3\n");
    }

    #[test]
    fn stat_across_omits_a_zero_total_and_pluralizes_the_unit() {
        let mut out = String::new();
        stat_across(&mut out, "Releases published", 0, 0, "repo");
        assert!(out.is_empty());
        stat_across(&mut out, "Commits authored", 12, 2, "repo");
        assert_eq!(out, "- **Commits authored:** 12 across 2 repo(s)\n");
    }
    use super::*;

    const GITLAB: TokenSpec = TokenSpec {
        service: "GitLab",
        generic_env: "GITLAB_TOKEN",
        host_separators: &['.'],
        hint: "",
    };

    #[test]
    fn date_in_range_is_inclusive_and_date_only() {
        let s = NaiveDate::from_ymd_opt(2026, 6, 1).unwrap();
        let u = NaiveDate::from_ymd_opt(2026, 6, 30).unwrap();
        assert!(date_in_range("2026-06-01T00:00:00Z", s, u));
        assert!(date_in_range("2026-06-30T23:59:59+02:00", s, u));
        assert!(!date_in_range("2026-07-01T00:00:00Z", s, u));
        assert!(!date_in_range("garbage", s, u));
    }

    #[test]
    fn instance_host_strips_scheme_and_slash() {
        assert_eq!(instance_host("https://gitlab.com/"), "gitlab.com");
        assert_eq!(instance_host("http://localhost:8080"), "localhost:8080");
    }

    #[test]
    fn instance_token_env_uses_spec_separators() {
        assert_eq!(
            instance_token_env("https://salsa.debian.org/", &GITLAB),
            "GITLAB_TOKEN_SALSA_DEBIAN_ORG"
        );
        let sourcehut = TokenSpec {
            host_separators: &['.', '-'],
            generic_env: "SOURCEHUT_TOKEN",
            ..GITLAB
        };
        assert_eq!(
            instance_token_env("git.sr-ht.test", &sourcehut),
            "SOURCEHUT_TOKEN_GIT_SR_HT_TEST"
        );
    }

    #[test]
    fn find_token_error_names_both_vars_and_hint() {
        let spec = TokenSpec {
            hint: " (generate at example.test)",
            ..GITLAB
        };
        // Skip if the env vars happen to be set — the fake
        // hostname keeps that unlikely, but a shell that exported
        // GITLAB_TOKEN would otherwise short-circuit the lookup.
        if std::env::var(instance_token_env(
            "https://nonexistent.example.test",
            &spec,
        ))
        .is_ok()
            || std::env::var("GITLAB_TOKEN").is_ok()
        {
            return;
        }
        let err =
            find_token("https://nonexistent.example.test", &BTreeMap::new(), &spec).unwrap_err();
        assert!(err.contains("no GitLab token for nonexistent.example.test"));
        assert!(err.contains("GITLAB_TOKEN_NONEXISTENT_EXAMPLE_TEST (instance-specific)"));
        assert!(err.contains("GITLAB_TOKEN (generic)"));
        assert!(err.ends_with("(generate at example.test)"));
    }
}
