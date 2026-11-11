// SPDX-License-Identifier: Apache-2.0 OR MIT

//! What callers' tests share, behind the `test-support` feature: a
//! caller lists `sandogasa-bugzilla` again under `[dev-dependencies]`
//! with `features = ["test-support"]`. For tests only: nothing here is
//! part of the crate's stable API.

/// The body Bugzilla answers a bug search with, given `bugs` and the
/// request's query string: each bug carries only the fields its
/// `include_fields` names. `_default` stands for every field of the bug
/// but `flags`, which Bugzilla's default set leaves out; no list at all
/// gives the whole bug. A test handing it complete bugs from its mock
/// server, whichever framework that is, then shows that the caller asks
/// for every field it reads, since one it forgot reads as empty, as
/// against the real server (#123). It answers every bug in one page,
/// whatever the limit and offset.
///
/// ```ignore
/// .respond_with(move |r: &wiremock::Request| {
///     wiremock::ResponseTemplate::new(200).set_body_json(answer(&bugs, r.url.query()))
/// })
/// ```
pub fn answer(bugs: &[serde_json::Value], query: Option<&str>) -> serde_json::Value {
    let fields = include_fields(query.unwrap_or_default());
    let bugs: Vec<serde_json::Value> = bugs
        .iter()
        .map(|bug| trimmed(bug, fields.as_deref()))
        .collect();
    serde_json::json!({ "total_matches": bugs.len(), "bugs": bugs })
}

/// The `include_fields` list of a query string, if it has one.
fn include_fields(query: &str) -> Option<Vec<String>> {
    let value = query
        .split('&')
        .find_map(|pair| pair.strip_prefix("include_fields="))?;
    Some(
        value
            .replace("%2C", ",")
            .replace("%2c", ",")
            .split(',')
            .map(str::to_string)
            .collect(),
    )
}

/// `bug` with only `fields`: whole for no list, and for `_default`
/// everything but `flags` unless that is asked for besides.
fn trimmed(bug: &serde_json::Value, fields: Option<&[String]>) -> serde_json::Value {
    let (Some(fields), Some(object)) = (fields, bug.as_object()) else {
        return bug.clone();
    };
    let asked = |k: &str| fields.iter().any(|f| f == k);
    let default = asked("_default");
    serde_json::Value::Object(
        object
            .iter()
            .filter(|(k, _)| asked(k) || (default && k.as_str() != "flags"))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bug_keeps_only_the_fields_asked_for() {
        let bug = serde_json::json!({"id": 1, "summary": "s", "blocks": [2], "flags": []});
        let body = answer(
            std::slice::from_ref(&bug),
            Some("component=x&include_fields=id,blocks"),
        );
        assert_eq!(body["bugs"][0], serde_json::json!({"id": 1, "blocks": [2]}));
        assert_eq!(body["total_matches"], 1);
        // Encoded commas read the same.
        let body = answer(
            std::slice::from_ref(&bug),
            Some("include_fields=id%2Csummary"),
        );
        assert_eq!(
            body["bugs"][0],
            serde_json::json!({"id": 1, "summary": "s"})
        );
        // No list: the whole bug.
        assert_eq!(answer(std::slice::from_ref(&bug), None)["bugs"][0], bug);
    }

    #[test]
    fn the_default_set_leaves_out_flags_unless_asked_for() {
        let bug = serde_json::json!({"id": 1, "summary": "s", "flags": [{"name": "x"}]});
        let body = answer(std::slice::from_ref(&bug), Some("include_fields=_default"));
        assert_eq!(
            body["bugs"][0],
            serde_json::json!({"id": 1, "summary": "s"})
        );
        let body = answer(
            std::slice::from_ref(&bug),
            Some("include_fields=_default,flags"),
        );
        assert_eq!(body["bugs"][0], bug);
    }

    #[tokio::test]
    async fn search_with_gets_only_required_and_extra_fields() {
        let server = wiremock::MockServer::start().await;
        let full = serde_json::json!({
            "id": 7, "summary": "s", "status": "NEW", "resolution": "",
            "product": "Fedora", "component": ["foo"], "severity": "low",
            "priority": "low", "assigned_to": "a", "creator": "c",
            "creation_time": "2026-01-01T00:00:00Z",
            "last_change_time": "2026-01-01T00:00:00Z",
            "blocks": [9], "keywords": ["Security"]
        });
        wiremock::Mock::given(wiremock::matchers::path("/rest/bug"))
            .respond_with(move |r: &wiremock::Request| {
                wiremock::ResponseTemplate::new(200)
                    .set_body_json(answer(std::slice::from_ref(&full), r.url.query()))
            })
            .mount(&server)
            .await;
        let bz = crate::BzClient::new(&server.uri());
        let bugs = bz
            .search_with("component=foo", 0, &["blocks"])
            .await
            .unwrap();
        assert_eq!(bugs[0].blocks, [9]);
        assert!(bugs[0].keywords.is_empty(), "not asked for, so not sent");
    }
}
