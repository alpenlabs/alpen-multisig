//! Orchestrator backend URL policy — shared by auth and proposal IPC paths.

const INSECURE_HTTP_ERROR: &str =
    "orchestrator base_url must use https:// (http://localhost or 127.0.0.1 allowed for local dev only)";

pub fn validate_orchestrator_base_url(base_url: &str) -> Result<(), String> {
    let trimmed = base_url.trim();
    if trimmed.starts_with("https://") {
        return Ok(());
    }
    if is_local_http(trimmed) {
        return Ok(());
    }
    let allow_insecure = std::env::var("ALLOW_INSECURE_HTTP_URL")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    if allow_insecure {
        return Ok(());
    }
    Err(INSECURE_HTTP_ERROR.to_string())
}

/// Plain HTTP to this machine: the host is exactly `localhost` or a loopback address. A prefix
/// test would also accept `http://localhost.example.com` and `http://127.0.0.1@remote.example`.
fn is_local_http(url: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(url) else {
        return false;
    };
    if url.scheme() != "http" || !url.username().is_empty() || url.password().is_some() {
        return false;
    }
    match url.host_str() {
        Some(h) if h.eq_ignore_ascii_case("localhost") => true,
        Some(h) => h
            .trim_matches(|c| c == '[' || c == ']')
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback()),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::validate_orchestrator_base_url;

    #[test]
    fn a_remote_host_that_only_looks_local_is_rejected() {
        for url in [
            "http://localhost.example.com/api/v1",
            "http://127.0.0.1@remote.example/api/v1",
            "http://127.0.0.1.remote.example/api/v1",
            "http://localhost:pw@remote.example/api/v1",
        ] {
            assert!(validate_orchestrator_base_url(url).is_err(), "{url}");
        }
        for url in [
            "http://localhost:3000/api/v1",
            "http://LOCALHOST/api/v1",
            "http://[::1]:3000/api/v1",
            "http://127.0.0.1/api/v1",
        ] {
            assert!(validate_orchestrator_base_url(url).is_ok(), "{url}");
        }
    }

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
}
