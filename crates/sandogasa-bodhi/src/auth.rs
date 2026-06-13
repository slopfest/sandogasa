// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Reuse the bodhi CLI's cached OIDC session.
//!
//! Bodhi's write API requires an OIDC bearer token. Rather than
//! implementing a browser login flow, we piggyback on the official
//! `bodhi` CLI (bodhi-client): it caches its tokens in
//! `~/.config/bodhi/client.json` under a `tokens` key, keyed by ID
//! provider URL. The user authenticates once with any
//! authenticated bodhi CLI command, and both tools share the
//! session. Expired access tokens are refreshed against the ID
//! provider's token endpoint (discovered via OIDC metadata) and
//! written back to the cache so the CLI benefits too.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The Fedora ID provider bodhi-client authenticates against.
pub const FEDORA_IDP: &str = "https://id.fedoraproject.org/openidc";

/// The OIDC client ID registered for bodhi-client. Used for token
/// refresh; the refresh token was issued to this client.
pub const BODHI_CLIENT_ID: &str = "bodhi-client";

/// The OIDC scopes bodhi-client requests (mirrors its `SCOPE`
/// constant). Sent with refresh requests too — omitting the scope
/// there makes the IdP issue a token without the identity scopes,
/// which Bodhi then rejects ("You must provide an author").
pub const BODHI_SCOPE: &str = "openid email profile \
     https://id.fedoraproject.org/scope/groups \
     https://id.fedoraproject.org/scope/agreements";

/// Slack subtracted from `expires_at` so we refresh slightly
/// before actual expiry rather than racing it.
const EXPIRY_SLACK_SECS: i64 = 60;

/// An OIDC token set as stored by bodhi-client (authlib format).
///
/// Unknown fields are preserved via `extra` so saving a refreshed
/// token back doesn't drop anything the Python client relies on.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OidcTokens {
    pub access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    /// Unix timestamp of access-token expiry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl OidcTokens {
    /// Whether the access token is expired (or about to be) at
    /// `now` (a Unix timestamp). Tokens without `expires_at` are
    /// assumed valid — a stale one surfaces as HTTP 401 instead.
    pub fn is_expired(&self, now: i64) -> bool {
        match self.expires_at {
            Some(at) => now >= at - EXPIRY_SLACK_SECS,
            None => false,
        }
    }
}

/// Default location of bodhi-client's token cache
/// (`~/.config/bodhi/client.json`).
pub fn cli_cache_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("~/.config"))
        .join("bodhi")
        .join("client.json")
}

/// Load the cached tokens for `id_provider` from a bodhi-client
/// cache file. Returns `Ok(None)` if the file or the provider
/// entry doesn't exist.
pub fn load_tokens(path: &Path, id_provider: &str) -> Result<Option<OidcTokens>, String> {
    if !path.exists() {
        return Ok(None);
    }
    let contents = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let store: serde_json::Value = serde_json::from_str(&contents)
        .map_err(|e| format!("cannot parse {}: {e}", path.display()))?;
    let Some(entry) = store.get("tokens").and_then(|t| t.get(id_provider)) else {
        return Ok(None);
    };
    let tokens: OidcTokens = serde_json::from_value(entry.clone())
        .map_err(|e| format!("unexpected token format in {}: {e}", path.display()))?;
    Ok(Some(tokens))
}

/// Save `tokens` for `id_provider` back into a bodhi-client cache
/// file, preserving any other keys and providers in the file.
pub fn save_tokens(path: &Path, id_provider: &str, tokens: &OidcTokens) -> Result<(), String> {
    let mut store: serde_json::Value = if path.exists() {
        let contents = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        serde_json::from_str(&contents)
            .map_err(|e| format!("cannot parse {}: {e}", path.display()))?
    } else {
        serde_json::json!({})
    };
    let value = serde_json::to_value(tokens).map_err(|e| e.to_string())?;
    store
        .as_object_mut()
        .ok_or_else(|| format!("{} is not a JSON object", path.display()))?
        .entry("tokens")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .ok_or_else(|| format!("'tokens' in {} is not a JSON object", path.display()))?
        .insert(id_provider.to_string(), value);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(
        path,
        serde_json::to_string(&store).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    Ok(())
}

/// Retry attempts for transient failures (transport errors and
/// 5xx) on the auth endpoints. The whole point of these requests
/// is usually to post something after a long-running analysis —
/// dying on a connection blip at the finish line wastes all of
/// that work.
const AUTH_RETRY_ATTEMPTS: u32 = 3;

/// Send a request (rebuilt fresh per attempt) with retries on
/// transport errors and 5xx responses. Other statuses return the
/// response for the caller to interpret. Only use with requests
/// that are safe to repeat.
pub(crate) async fn send_with_retry<F>(what: &str, build: F) -> Result<reqwest::Response, String>
where
    F: Fn() -> reqwest::RequestBuilder,
{
    let mut last_err = String::new();
    for attempt in 0..AUTH_RETRY_ATTEMPTS {
        if attempt > 0 {
            let delay = std::time::Duration::from_secs(1 << (attempt - 1));
            eprintln!(
                "{what}: {last_err}; retrying in {}s ({attempt}/{})",
                delay.as_secs(),
                AUTH_RETRY_ATTEMPTS - 1
            );
            tokio::time::sleep(delay).await;
        }
        match build().send().await {
            Ok(resp) if resp.status().is_server_error() => {
                last_err = format!("HTTP {}", resp.status());
            }
            Ok(resp) => return Ok(resp),
            Err(e) => last_err = format!("{e}"),
        }
    }
    Err(format!("{what} failed after retries: {last_err}"))
}

#[derive(Deserialize)]
struct OidcMetadata {
    token_endpoint: Option<String>,
    userinfo_endpoint: Option<String>,
}

/// Fetch the OIDC discovery document, with retries.
async fn fetch_metadata(http: &reqwest::Client, id_provider: &str) -> Result<OidcMetadata, String> {
    let metadata_url = format!(
        "{}/.well-known/openid-configuration",
        id_provider.trim_end_matches('/')
    );
    send_with_retry("OIDC metadata fetch", || http.get(&metadata_url))
        .await?
        .error_for_status()
        .map_err(|e| format!("OIDC metadata request failed: {e}"))?
        .json()
        .await
        .map_err(|e| format!("cannot parse OIDC metadata: {e}"))
}

/// Refresh an OIDC token set against `id_provider`'s token
/// endpoint (discovered via `.well-known/openid-configuration`).
///
/// `scope` must repeat the scopes of the original grant (e.g.
/// [`BODHI_SCOPE`]): some providers (Fedora's Ipsilon included)
/// otherwise issue a refreshed token without the identity scopes,
/// which the API then treats as anonymous.
pub async fn refresh_tokens(
    http: &reqwest::Client,
    id_provider: &str,
    client_id: &str,
    scope: &str,
    refresh_token: &str,
) -> Result<OidcTokens, String> {
    let token_endpoint = fetch_metadata(http, id_provider)
        .await?
        .token_endpoint
        .ok_or("OIDC metadata has no token_endpoint")?;

    // Retried like the GETs: a token refresh is repeatable (the
    // worst case with a rotating provider is a dead session that
    // needs one interactive re-login, vs. certainly losing the
    // whole run to a connection blip).
    let resp = send_with_retry("token refresh", || {
        http.post(&token_endpoint).form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", client_id),
            ("scope", scope),
        ])
    })
    .await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!(
            "token refresh failed (HTTP {status}): {body}; \
             re-authenticate with the bodhi CLI"
        ));
    }
    let mut tokens: OidcTokens = resp
        .json()
        .await
        .map_err(|e| format!("cannot parse refreshed tokens: {e}"))?;
    // Token endpoints return expires_in (relative); compute
    // expires_at the way authlib does so is_expired keeps working.
    if tokens.expires_at.is_none()
        && let Some(expires_in) = tokens.extra.get("expires_in").and_then(|v| v.as_i64())
    {
        tokens.expires_at = Some(chrono::Utc::now().timestamp() + expires_in);
    }
    // Some providers omit the refresh token on refresh; keep using
    // the old one in that case.
    if tokens.refresh_token.is_none() {
        tokens.refresh_token = Some(refresh_token.to_string());
    }
    Ok(tokens)
}

/// Fetch the username behind an access token from the ID
/// provider's UserInfo endpoint (the `nickname` claim, which is
/// the FAS username — same as bodhi-client's `username`).
pub async fn username(
    http: &reqwest::Client,
    id_provider: &str,
    access_token: &str,
) -> Result<String, String> {
    #[derive(Deserialize)]
    struct UserInfo {
        nickname: String,
    }
    let userinfo_endpoint = fetch_metadata(http, id_provider)
        .await?
        .userinfo_endpoint
        .ok_or("OIDC metadata has no userinfo_endpoint")?;
    let info: UserInfo = send_with_retry("userinfo request", || {
        http.get(&userinfo_endpoint).bearer_auth(access_token)
    })
    .await?
    .error_for_status()
    .map_err(|e| format!("userinfo request failed: {e}"))?
    .json()
    .await
    .map_err(|e| format!("cannot parse userinfo: {e}"))?;
    Ok(info.nickname)
}

/// Get a valid access token from the bodhi CLI's session cache at
/// `path`, refreshing (and saving back) if expired.
///
/// Errors if there is no cached session — the user must
/// authenticate once with the bodhi CLI first.
pub async fn cli_session_token(
    http: &reqwest::Client,
    path: &Path,
    id_provider: &str,
) -> Result<String, String> {
    session_token_impl(http, path, id_provider, false).await
}

/// Like [`cli_session_token`], but refreshes preemptively even if
/// the cached access token hasn't expired yet. Use right before a
/// write request — especially one following a long-running phase —
/// to minimize the risk of the token expiring mid-flight. Falls
/// back to a still-valid cached token when the session has no
/// refresh token.
pub async fn cli_session_token_refreshed(
    http: &reqwest::Client,
    path: &Path,
    id_provider: &str,
) -> Result<String, String> {
    session_token_impl(http, path, id_provider, true).await
}

async fn session_token_impl(
    http: &reqwest::Client,
    path: &Path,
    id_provider: &str,
    force_refresh: bool,
) -> Result<String, String> {
    let Some(tokens) = load_tokens(path, id_provider)? else {
        return Err(format!(
            "no bodhi CLI session found in {}; authenticate once \
             with the bodhi CLI (any authenticated command, e.g. \
             `bodhi overrides query --mine`) and retry",
            path.display()
        ));
    };
    let expired = tokens.is_expired(chrono::Utc::now().timestamp());
    if !expired && !force_refresh {
        return Ok(tokens.access_token);
    }
    let Some(ref refresh_token) = tokens.refresh_token else {
        if !expired {
            return Ok(tokens.access_token);
        }
        return Err("bodhi CLI session is expired and has no refresh token; \
             re-authenticate with the bodhi CLI"
            .to_string());
    };
    let refreshed = refresh_tokens(
        http,
        id_provider,
        BODHI_CLIENT_ID,
        BODHI_SCOPE,
        refresh_token,
    )
    .await?;
    save_tokens(path, id_provider, &refreshed)?;
    Ok(refreshed.access_token)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn sample_store(expires_at: i64) -> String {
        serde_json::json!({
            "tokens": {
                FEDORA_IDP: {
                    "access_token": "abc123",
                    "refresh_token": "refresh456",
                    "expires_at": expires_at,
                    "token_type": "Bearer",
                    "expires_in": 240
                }
            }
        })
        .to_string()
    }

    #[test]
    fn load_tokens_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("client.json");
        assert!(load_tokens(&path, FEDORA_IDP).unwrap().is_none());
    }

    #[test]
    fn load_tokens_missing_provider() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("client.json");
        std::fs::write(&path, sample_store(99)).unwrap();
        assert!(
            load_tokens(&path, "https://other.example")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn load_tokens_roundtrip_preserves_extra_fields() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("client.json");
        std::fs::write(&path, sample_store(1700000000)).unwrap();

        let tokens = load_tokens(&path, FEDORA_IDP).unwrap().unwrap();
        assert_eq!(tokens.access_token, "abc123");
        assert_eq!(tokens.refresh_token.as_deref(), Some("refresh456"));
        assert_eq!(tokens.expires_at, Some(1700000000));

        save_tokens(&path, FEDORA_IDP, &tokens).unwrap();
        let store: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        // authlib fields the Python client needs survive the trip.
        assert_eq!(
            store["tokens"][FEDORA_IDP]["token_type"],
            serde_json::json!("Bearer")
        );
        assert_eq!(
            store["tokens"][FEDORA_IDP]["expires_in"],
            serde_json::json!(240)
        );
    }

    #[test]
    fn save_tokens_preserves_other_providers_and_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("client.json");
        std::fs::write(
            &path,
            serde_json::json!({
                "tokens": {"https://other.example": {"access_token": "zzz"}},
                "unrelated": 42
            })
            .to_string(),
        )
        .unwrap();

        let tokens = OidcTokens {
            access_token: "new".into(),
            refresh_token: None,
            expires_at: None,
            extra: serde_json::Map::new(),
        };
        save_tokens(&path, FEDORA_IDP, &tokens).unwrap();

        let store: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(store["unrelated"], serde_json::json!(42));
        assert_eq!(
            store["tokens"]["https://other.example"]["access_token"],
            serde_json::json!("zzz")
        );
        assert_eq!(
            store["tokens"][FEDORA_IDP]["access_token"],
            serde_json::json!("new")
        );
    }

    #[test]
    fn is_expired_slack_and_missing() {
        let tokens = OidcTokens {
            access_token: "x".into(),
            refresh_token: None,
            expires_at: Some(1000),
            extra: serde_json::Map::new(),
        };
        assert!(!tokens.is_expired(900));
        assert!(tokens.is_expired(945)); // within 60s slack
        assert!(tokens.is_expired(1001));

        let no_expiry = OidcTokens {
            expires_at: None,
            ..tokens
        };
        assert!(!no_expiry.is_expired(i64::MAX));
    }

    #[tokio::test(start_paused = true)]
    async fn send_with_retry_retries_transport_errors() {
        // Nothing listens on port 1: every attempt fails at the
        // transport level. Paused time fast-forwards the backoff
        // sleeps; the error names the operation after retries.
        sandogasa_cli::install_crypto_provider();
        let http = reqwest::Client::new();
        let err = send_with_retry("token refresh", || http.post("http://127.0.0.1:1/Token"))
            .await
            .unwrap_err();
        assert!(err.contains("token refresh failed after retries"), "{err}");
    }

    #[tokio::test]
    async fn send_with_retry_recovers_from_5xx() {
        let server = MockServer::start().await;
        // First attempt: 500. Subsequent: 200.
        Mock::given(method("GET"))
            .and(path("/flaky"))
            .respond_with(ResponseTemplate::new(500))
            .up_to_n_times(1)
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/flaky"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        sandogasa_cli::install_crypto_provider();
        let http = reqwest::Client::new();
        let url = format!("{}/flaky", server.uri());
        let resp = send_with_retry("flaky fetch", || http.get(&url))
            .await
            .unwrap();
        assert!(resp.status().is_success());
    }

    #[tokio::test]
    async fn refresh_tokens_uses_discovered_endpoint() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/.well-known/openid-configuration"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "token_endpoint": format!("{}/Token", server.uri())
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/Token"))
            .and(body_string_contains("grant_type=refresh_token"))
            .and(body_string_contains("refresh_token=old-refresh"))
            .and(body_string_contains("client_id=bodhi-client"))
            .and(body_string_contains("scope=openid+email+profile"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "fresh",
                "token_type": "Bearer",
                "expires_in": 240
            })))
            .expect(1)
            .mount(&server)
            .await;

        sandogasa_cli::install_crypto_provider();
        let http = reqwest::Client::new();
        let tokens = refresh_tokens(
            &http,
            &server.uri(),
            BODHI_CLIENT_ID,
            BODHI_SCOPE,
            "old-refresh",
        )
        .await
        .unwrap();
        assert_eq!(tokens.access_token, "fresh");
        // expires_at computed from expires_in.
        assert!(tokens.expires_at.is_some());
        // refresh token retained when the provider omits it.
        assert_eq!(tokens.refresh_token.as_deref(), Some("old-refresh"));
    }

    #[tokio::test]
    async fn cli_session_token_refreshes_expired_and_saves() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/.well-known/openid-configuration"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "token_endpoint": format!("{}/Token", server.uri())
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/Token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "fresh",
                "refresh_token": "new-refresh",
                "expires_in": 240
            })))
            .mount(&server)
            .await;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("client.json");
        // expires_at far in the past -> must refresh.
        let store = serde_json::json!({
            "tokens": {
                server.uri(): {
                    "access_token": "stale",
                    "refresh_token": "old-refresh",
                    "expires_at": 1000
                }
            }
        });
        std::fs::write(&path, store.to_string()).unwrap();

        sandogasa_cli::install_crypto_provider();
        let http = reqwest::Client::new();
        let token = cli_session_token(&http, &path, &server.uri())
            .await
            .unwrap();
        assert_eq!(token, "fresh");

        // Refreshed tokens written back for the bodhi CLI to reuse.
        let saved = load_tokens(&path, &server.uri()).unwrap().unwrap();
        assert_eq!(saved.access_token, "fresh");
        assert_eq!(saved.refresh_token.as_deref(), Some("new-refresh"));
    }

    #[tokio::test]
    async fn cli_session_token_refreshed_refreshes_valid_token() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/.well-known/openid-configuration"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "token_endpoint": format!("{}/Token", server.uri())
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/Token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "fresh",
                "refresh_token": "new-refresh",
                "expires_in": 240
            })))
            .expect(1)
            .mount(&server)
            .await;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("client.json");
        // Token valid for another hour — refreshed anyway.
        let future = chrono::Utc::now().timestamp() + 3600;
        let store = serde_json::json!({
            "tokens": {
                server.uri(): {
                    "access_token": "still-valid",
                    "refresh_token": "old-refresh",
                    "expires_at": future
                }
            }
        });
        std::fs::write(&path, store.to_string()).unwrap();

        sandogasa_cli::install_crypto_provider();
        let http = reqwest::Client::new();
        let token = cli_session_token_refreshed(&http, &path, &server.uri())
            .await
            .unwrap();
        assert_eq!(token, "fresh");
        let saved = load_tokens(&path, &server.uri()).unwrap().unwrap();
        assert_eq!(saved.refresh_token.as_deref(), Some("new-refresh"));
    }

    #[tokio::test]
    async fn cli_session_token_refreshed_falls_back_without_refresh_token() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("client.json");
        let future = chrono::Utc::now().timestamp() + 3600;
        let store = serde_json::json!({
            "tokens": {
                FEDORA_IDP: {
                    "access_token": "still-valid",
                    "expires_at": future
                }
            }
        });
        std::fs::write(&path, store.to_string()).unwrap();
        sandogasa_cli::install_crypto_provider();
        let http = reqwest::Client::new();
        let token = cli_session_token_refreshed(&http, &path, FEDORA_IDP)
            .await
            .unwrap();
        assert_eq!(token, "still-valid");
    }

    #[tokio::test]
    async fn cli_session_token_errors_without_cache() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("client.json");
        sandogasa_cli::install_crypto_provider();
        let http = reqwest::Client::new();
        let err = cli_session_token(&http, &path, FEDORA_IDP)
            .await
            .unwrap_err();
        assert!(
            err.contains("authenticate once with the bodhi CLI"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn cli_session_token_uses_valid_token_without_refresh() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("client.json");
        let future = chrono::Utc::now().timestamp() + 3600;
        std::fs::write(&path, sample_store(future)).unwrap();
        sandogasa_cli::install_crypto_provider();
        let http = reqwest::Client::new();
        let token = cli_session_token(&http, &path, FEDORA_IDP).await.unwrap();
        assert_eq!(token, "abc123");
    }
}
