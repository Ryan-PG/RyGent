//! Provider connectivity check behind the "Test" button (spec sections 12, 16).
//!
//! Issues an authenticated `GET` against the provider's base URL and reports a
//! structured, **secret-free** result. Two rules drive every decision here
//! (spec sections 5, 17):
//!
//! 1. The API key is only ever placed in request headers. It is never logged,
//!    never returned, and never included in an error message.
//! 2. Response bodies are never echoed back to the caller - a gateway error
//!    page can contain reflected credentials, and the UI only needs a status.
//!
//! The credential is sent as `Authorization: Bearer <key>` and nothing else
//! (spec section 5): that is how a session authenticates the provider key
//! (`ANTHROPIC_AUTH_TOKEN`), and the test must preflight what the session will
//! actually send. No `x-api-key` header is added, because presenting both
//! credential styles at once is what makes a bearer-authenticating gateway
//! answer HTTP 401.

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Request timeout. Interactive UI: long enough for a slow gateway, short
/// enough that the "Test" button always answers.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Outcome of a provider connectivity check, shown inline in the UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderTestResult {
    /// Whether the endpoint answered successfully.
    pub ok: bool,
    /// HTTP status of the successful/failing attempt, when a response arrived.
    /// `None` means the request never completed (DNS, connection, TLS, TLS
    /// timeout, or a malformed URL).
    pub status: Option<u16>,
    /// Short, user-facing, secret-free description.
    pub message: String,
}

impl ProviderTestResult {
    fn reachable(status: u16, label: &str) -> Self {
        Self {
            ok: true,
            status: Some(status),
            message: format!("reachable ({label})"),
        }
    }

    fn rejected(status: u16, label: &str) -> Self {
        Self {
            ok: false,
            status: Some(status),
            message: format!("endpoint answered {label} - check the base URL and API key"),
        }
    }

    fn unreachable(reason: &str) -> Self {
        Self {
            ok: false,
            status: None,
            message: format!("could not reach the endpoint: {reason}"),
        }
    }
}

/// Check whether `base_url` answers an authenticated request.
///
/// The base URL is tried first; when it does not answer successfully the
/// conventional `/v1/models` listing is tried as a fallback, because many
/// Anthropic/OpenAI-compatible gateways do not serve the bare root path.
/// `api_key` is `None` when no secret is stored, in which case the request is
/// sent unauthenticated (some local gateways need no key).
pub async fn check(base_url: &str, api_key: Option<&str>) -> ProviderTestResult {
    let client = match reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        // Follows redirects by default; a gateway that redirects to a login
        // page will surface as a non-2xx status, which is the honest answer.
        .build()
    {
        Ok(client) => client,
        Err(error) => return ProviderTestResult::unreachable(transport_reason(&error)),
    };

    let mut failure: Option<ProviderTestResult> = None;

    for endpoint in candidates(base_url) {
        let request = authenticated(client.get(endpoint.as_str()), api_key);

        match request.send().await {
            Ok(response) => {
                let status = response.status();
                // The body is intentionally dropped without being read: it is
                // not needed, and it must never be logged or returned.
                drop(response);
                let code = status.as_u16();
                if status.is_success() {
                    return ProviderTestResult::reachable(code, &status_label(status));
                }
                failure = Some(ProviderTestResult::rejected(code, &status_label(status)));
            }
            Err(error) => {
                // Transport failures are not retried against the fallback
                // path: if the host does not answer, neither URL will.
                return ProviderTestResult::unreachable(transport_reason(&error));
            }
        }
    }

    failure.unwrap_or_else(|| ProviderTestResult::unreachable("no endpoint to test"))
}

/// Apply the credential to a request, the way a session authenticates.
///
/// `Authorization: Bearer <key>` and the API version header, and nothing else:
/// `x-api-key` is deliberately absent (see the module docs). Split out from
/// [`check`] so this header contract is asserted directly, on a built request,
/// without a network round trip - the key never enters the URL, the logs, or
/// the result, and the returned request is only handed to the HTTP client.
fn authenticated(
    request: reqwest::RequestBuilder,
    api_key: Option<&str>,
) -> reqwest::RequestBuilder {
    match api_key {
        Some(api_key) => request
            .bearer_auth(api_key)
            .header("anthropic-version", "2023-06-01"),
        // No stored key: some local gateways need none.
        None => request,
    }
}

/// Endpoints to try, in order: the base URL itself, then the conventional
/// model-listing path (unless the base URL already points at it).
fn candidates(base_url: &str) -> Vec<String> {
    let base = base_url.trim().trim_end_matches('/').to_string();
    let mut endpoints = vec![base.clone()];
    if !base.ends_with("/v1/models") && !base.ends_with("/models") {
        endpoints.push(format!("{base}/v1/models"));
    }
    endpoints
}

/// `HTTP 401 Unauthorized` style label built from the status code only - the
/// response body is never inspected.
fn status_label(status: reqwest::StatusCode) -> String {
    match status.canonical_reason() {
        Some(reason) => format!("HTTP {} {reason}", status.as_u16()),
        None => format!("HTTP {}", status.as_u16()),
    }
}

/// Classify a transport error without echoing it.
///
/// `reqwest::Error`'s `Display` includes the request URL, which is user input
/// and may carry credentials (a base URL like `https://user:key@host` is
/// rejected at validation time, but defense in depth is cheap). Only a coarse
/// category is reported, so no part of the request can leak into the message.
///
/// The categories are also what makes the failure actionable (spec section 16):
/// "the request could not be built" points at the base URL or the API key,
/// which is exactly the case where the credential itself is unusable.
fn transport_reason(error: &reqwest::Error) -> &'static str {
    if error.is_timeout() {
        "timed out after 10s"
    } else if error.is_connect() {
        "connection failed (host unreachable, DNS, or TLS error)"
    } else if error.is_builder() {
        // Raised before anything is sent: an unparseable base URL, or a
        // credential that cannot be a header value (an embedded newline, for
        // example). This is the "invalid API key" / "invalid Base URL" case.
        "invalid request: check the base URL and the API key (a key must be a single line)"
    } else if error.is_request() {
        "request could not be sent (invalid URL or header)"
    } else if error.is_decode() {
        "invalid response"
    } else if error.is_redirect() {
        "too many redirects"
    } else {
        "request failed"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidates_prefer_the_base_url_then_the_model_listing() {
        assert_eq!(
            candidates("https://provider-a.example.com"),
            vec![
                "https://provider-a.example.com",
                "https://provider-a.example.com/v1/models",
            ]
        );

        // Trailing slashes and surrounding whitespace are normalized away.
        assert_eq!(
            candidates("  https://provider-a.example.com/  "),
            vec![
                "https://provider-a.example.com",
                "https://provider-a.example.com/v1/models",
            ]
        );
    }

    #[test]
    fn candidates_do_not_duplicate_an_explicit_models_path() {
        assert_eq!(
            candidates("https://provider-a.example.com/v1/models"),
            vec!["https://provider-a.example.com/v1/models"]
        );
        assert_eq!(
            candidates("https://provider-a.example.com/models/"),
            vec!["https://provider-a.example.com/models"]
        );
    }

    #[test]
    fn results_are_serialized_camel_case_for_the_frontend() {
        let json = serde_json::to_value(ProviderTestResult::reachable(200, "HTTP 200 OK")).unwrap();
        assert_eq!(json["ok"], serde_json::json!(true));
        assert_eq!(json["status"], serde_json::json!(200));
        assert_eq!(json["message"], serde_json::json!("reachable (HTTP 200 OK)"));
    }

    // --- the credential header contract (spec section 5) --------------------
    //
    // Asserted on a *built* request through the same `authenticated` helper
    // `check` uses, so the header set the connectivity test sends is pinned
    // without a network round trip - and it is the same header set a session
    // presents (bearer token only).

    #[test]
    fn the_credential_is_sent_as_a_bearer_token_and_nothing_else() {
        let request = authenticated(
            reqwest::Client::new().get("https://provider-a.example.com"),
            Some(KEY),
        )
        .build()
        .expect("build the request");

        let headers = request.headers();
        assert_eq!(
            headers.get("authorization").expect("an authorization header"),
            &format!("Bearer {KEY}")
        );
        // The API-key header style is deliberately not sent: a gateway that
        // authenticates the bearer token answers HTTP 401 when both are
        // presented, which is the reported failure.
        assert!(
            headers.get("x-api-key").is_none(),
            "x-api-key must not be sent alongside the bearer token"
        );
        assert_eq!(
            headers.get("anthropic-version").expect("the API version"),
            "2023-06-01"
        );
        // Header-only: the credential never enters the URL (which is what gets
        // logged by proxies and echoed by `reqwest::Error`).
        assert!(!request.url().as_str().contains(KEY));
    }

    #[test]
    fn no_stored_credential_sends_no_authorization_header() {
        let request = authenticated(
            reqwest::Client::new().get("http://127.0.0.1:1"),
            None,
        )
        .build()
        .expect("build the request");

        assert!(request.headers().get("authorization").is_none());
        assert!(request.headers().get("x-api-key").is_none());
    }

    #[test]
    fn failure_messages_describe_the_status_without_a_response_body() {
        let rejected = ProviderTestResult::rejected(401, "HTTP 401 Unauthorized");
        assert!(!rejected.ok);
        assert_eq!(rejected.status, Some(401));
        assert!(rejected.message.contains("HTTP 401 Unauthorized"));

        let unreachable = ProviderTestResult::unreachable("connection failed");
        assert!(!unreachable.ok);
        assert_eq!(unreachable.status, None);
        assert!(!unreachable.message.is_empty());
    }

    // --- offline failure paths (spec sections 16, 17) ------------------------
    //
    // Everything below fails before or during the request without a network
    // round trip: a URL that cannot be parsed, a credential that cannot be a
    // header value, and a loopback port nothing listens on. That makes the
    // "invalid API key" / "invalid Base URL" / "unreachable" messages testable
    // in a suite that must not depend on the internet, and it is where a
    // credential is most likely to be echoed by accident.

    /// A value that looks like a credential. If it appears in a message, the
    /// test that asserts on it fails (spec section 17).
    const KEY: &str = "sk-DO-NOT-LEAK-9c41fb7e";

    /// A loopback TCP port nothing can be listening on.
    const DEAD_ENDPOINT: &str = "http://127.0.0.1:1";

    /// Assert the common contract of a failed check: not ok, no HTTP status,
    /// an explanation, and no credential or URL echoed anywhere in it.
    fn assert_clean_failure(result: &ProviderTestResult, secret: &str) {
        assert!(!result.ok, "a failure must not be reported as reachable");
        assert_eq!(result.status, None, "nothing answered, so there is no status");
        assert!(
            result.message.starts_with("could not reach the endpoint: "),
            "unexpected message: {}",
            result.message
        );
        assert!(!result.message.is_empty());
        assert!(
            !result.message.contains(secret),
            "the message echoed the credential: {}",
            result.message
        );
        assert!(
            !format!("{result:?}").contains(secret),
            "Debug output echoed the credential: {result:?}"
        );
    }

    #[tokio::test]
    async fn an_api_key_that_cannot_be_sent_is_reported_against_the_key_and_url() {
        // A key containing a newline cannot become an HTTP header value, which
        // is the closest thing to "the API key itself is unusable" that can be
        // detected without contacting the provider.
        let result = check(DEAD_ENDPOINT, Some(&format!("{KEY}\ninjected"))).await;

        assert_clean_failure(&result, KEY);
        assert!(
            result.message.contains("base URL") && result.message.contains("API key"),
            "the message must point at what to check: {}",
            result.message
        );
    }

    #[tokio::test]
    async fn an_unreachable_endpoint_reports_a_connection_failure_without_the_url() {
        let result = check(DEAD_ENDPOINT, Some(KEY)).await;

        assert_clean_failure(&result, KEY);
        assert!(
            result.message.contains("connection failed"),
            "unexpected message: {}",
            result.message
        );
        assert!(
            !result.message.contains(DEAD_ENDPOINT),
            "the request URL must not be echoed: {}",
            result.message
        );
    }

    #[tokio::test]
    async fn credentials_embedded_in_the_base_url_never_reach_the_message() {
        // Provider validation refuses a `user:password@host` base URL, so this
        // is the defense-in-depth check: even if one reached the HTTP client,
        // `reqwest::Error`'s own text (which contains the URL) is never used.
        let embedded = format!("{KEY}@127.0.0.1:1");
        let result = check(&format!("http://user:{embedded}"), Some(KEY)).await;

        assert_clean_failure(&result, KEY);
        assert!(
            !result.message.contains(&embedded),
            "the URL with credentials must not be echoed: {}",
            result.message
        );
    }

    #[tokio::test]
    async fn a_malformed_base_url_is_reported_rather_than_panicking() {
        let result = check("not a url", Some(KEY)).await;

        assert_clean_failure(&result, KEY);
        assert!(
            !result.message.contains("not a url"),
            "the malformed URL must not be echoed: {}",
            result.message
        );
    }
}
