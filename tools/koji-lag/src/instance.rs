// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Known Koji instances and hub-URL resolution.
//!
//! Build and task IDs are only unique per instance, so every
//! dataset record carries the instance key resolved here.

/// A known Koji instance.
pub struct Instance {
    pub name: &'static str,
    pub hub_url: &'static str,
}

/// Known instances (alphabetical). Only `fedora` has been
/// validated live; `cbs` and `stream` run older hub versions that
/// may omit or rename listTasks fields (tracked in TODO.md).
pub const INSTANCES: &[Instance] = &[
    Instance {
        name: "cbs",
        hub_url: "https://cbs.centos.org/kojihub",
    },
    Instance {
        name: "fedora",
        hub_url: "https://koji.fedoraproject.org/kojihub",
    },
    Instance {
        name: "stream",
        hub_url: "https://kojihub.stream.centos.org/kojihub",
    },
];

/// Resolve `--instance NAME` / `--hub-url URL` to the dataset
/// instance key and the hub URL. An explicit URL wins and uses its
/// host as the instance key.
pub fn resolve(name: &str, hub_url: Option<&str>) -> Result<(String, String), String> {
    if let Some(url) = hub_url {
        sandogasa_cli::ensure_secure_url(url)?;
        let host = url
            .strip_prefix("https://")
            .and_then(|rest| rest.split('/').next())
            .filter(|h| !h.is_empty())
            .ok_or_else(|| format!("cannot extract a host from --hub-url {url}"))?;
        return Ok((host.to_string(), url.trim_end_matches('/').to_string()));
    }
    INSTANCES
        .iter()
        .find(|i| i.name == name)
        .map(|i| (i.name.to_string(), i.hub_url.to_string()))
        .ok_or_else(|| {
            let known: Vec<&str> = INSTANCES.iter().map(|i| i.name).collect();
            format!(
                "unknown instance '{name}' (known: {}); or pass --hub-url",
                known.join(", ")
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_known_instance() {
        let (key, url) = resolve("fedora", None).unwrap();
        assert_eq!(key, "fedora");
        assert_eq!(url, "https://koji.fedoraproject.org/kojihub");
    }

    #[test]
    fn resolve_unknown_instance_lists_known() {
        let err = resolve("nope", None).unwrap_err();
        assert!(err.contains("cbs, fedora, stream"), "{err}");
    }

    #[test]
    fn hub_url_override_wins_and_keys_by_host() {
        let (key, url) = resolve("fedora", Some("https://koji.example.org/kojihub/")).unwrap();
        assert_eq!(key, "koji.example.org");
        assert_eq!(url, "https://koji.example.org/kojihub");
    }

    #[test]
    fn insecure_hub_url_is_rejected() {
        assert!(resolve("fedora", Some("http://koji.example.org/kojihub")).is_err());
    }
}
