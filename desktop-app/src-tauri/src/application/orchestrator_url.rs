//! Orchestrator backend URL policy — shared by auth and proposal IPC paths.

use std::net::IpAddr;

const INSECURE_HTTP_ERROR: &str =
    "orchestrator base_url must use https:// (http://localhost or 127.0.0.1 allowed for local dev only)";
const INVALID_URL_ERROR: &str =
    "orchestrator base_url must be an absolute HTTP(S) URL without credentials, query, or fragment";

pub fn validate_orchestrator_base_url(base_url: &str) -> Result<(), String> {
    normalize_orchestrator_base_url(base_url).map(|_| ())
}

/// Parse and canonicalize a renderer-provided orchestrator URL before it crosses an auth boundary.
pub fn normalize_orchestrator_base_url(base_url: &str) -> Result<String, String> {
    let allow_insecure = std::env::var("ALLOW_INSECURE_HTTP_URL")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    normalize_orchestrator_base_url_with_policy(base_url, allow_insecure)
}

fn normalize_orchestrator_base_url_with_policy(
    base_url: &str,
    allow_insecure: bool,
) -> Result<String, String> {
    let trimmed = base_url.trim();
    let parsed = reqwest::Url::parse(trimmed).map_err(|_| INVALID_URL_ERROR.to_string())?;
    if parsed.cannot_be_a_base()
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err(INVALID_URL_ERROR.to_string());
    }

    match parsed.scheme() {
        "https" => {}
        "http" if is_loopback_host(&parsed) || allow_insecure => {}
        "http" => return Err(INSECURE_HTTP_ERROR.to_string()),
        _ => return Err(INVALID_URL_ERROR.to_string()),
    }

    // Queries and fragments are rejected above, so trimming separators here only normalizes the
    // base path and cannot reinterpret any other URL component.
    Ok(parsed.to_string().trim_end_matches('/').to_string())
}

fn is_loopback_host(url: &reqwest::Url) -> bool {
    let Some(host) = url.host_str() else {
        return false;
    };
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    host.trim_start_matches('[')
        .trim_end_matches(']')
        .parse::<IpAddr>()
        .is_ok_and(|address| address.is_loopback())
}

#[cfg(test)]
mod tests {
    use super::{normalize_orchestrator_base_url_with_policy, validate_orchestrator_base_url};

    #[test]
    fn rejects_plain_http_remote() {
        assert!(validate_orchestrator_base_url("http://evil.example/api/v1").is_err());
    }

    #[test]
    fn allows_https() {
        assert!(validate_orchestrator_base_url("https://orchestrator.example/api/v1").is_ok());
    }

    #[test]
    fn allows_localhost_http() {
        assert!(validate_orchestrator_base_url("http://127.0.0.1:3000/api/v1").is_ok());
    }

    #[test]
    fn rejects_loopback_prefix_lookalikes() {
        for url in [
            "http://localhost.evil.example/api/v1",
            "http://127.0.0.1.evil.example/api/v1",
            "http://localhost@evil.example/api/v1",
        ] {
            assert!(normalize_orchestrator_base_url_with_policy(url, false).is_err());
        }
    }

    #[test]
    fn canonicalizes_equivalent_base_urls() {
        let first = normalize_orchestrator_base_url_with_policy(
            " https://ORCHESTRATOR.EXAMPLE:443/api/v1/ ",
            false,
        )
        .unwrap();
        let second = normalize_orchestrator_base_url_with_policy(
            "https://orchestrator.example/api/v1",
            false,
        )
        .unwrap();
        assert_eq!(first, second);
    }
}
