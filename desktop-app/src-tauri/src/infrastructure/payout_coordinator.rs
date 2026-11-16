//! HTTP client for the coordinator's `/payouts/*` routes (Payout Administrator session).

use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::infrastructure::rpc_timeout;

pub use payout_admin_core::wire::PayoutConfigView as CoordinatorConfig;

#[derive(Debug, thiserror::Error)]
pub enum CoordinatorError {
    #[error("The coordinator could not be reached: {0}")]
    Unreachable(String),
    #[error("The coordinator refused the request ({status}): {message}")]
    Refused { status: u16, message: String },
    #[error("Unexpected coordinator response: {0}")]
    Decode(String),
}

impl CoordinatorError {
    pub fn status(&self) -> Option<u16> {
        match self {
            Self::Refused { status, .. } => Some(*status),
            _ => None,
        }
    }
}

#[derive(Clone)]
pub struct PayoutCoordinator {
    base_url: String,
    token: String,
    client: reqwest::Client,
}

/// Largest coordinator answer this app reads. The Past list is not paginated and carries a PSBT
/// and a raw transaction per payout (a few kB, up to about 100 kB for a full one), so the budget
/// must hold years of history: it bounds memory against a hostile coordinator without becoming a
/// limit on an honest one.
const MAX_RESPONSE_BYTES: usize = 256 * 1024 * 1024;

/// The response body as text, reading at most `max` bytes whatever the server announces.
async fn read_limited(mut resp: reqwest::Response, max: usize) -> Result<String, CoordinatorError> {
    let mut body = Vec::new();
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| CoordinatorError::Unreachable(e.to_string()))?
    {
        if body.len() + chunk.len() > max {
            return Err(CoordinatorError::Decode(format!(
                "the coordinator's answer is larger than {} MiB",
                max / (1024 * 1024)
            )));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(String::from_utf8_lossy(&body).into_owned())
}

// Each route retains its typed method; transport and error handling stay in `send`.
macro_rules! routes {
    ($($name:ident($($arg:ident: $ty:ty),*) -> $response:ty:
        $method:ident $path:expr, $body:expr;)*) => {
        $(pub async fn $name(&self, $($arg: $ty),*) -> Result<$response, CoordinatorError> {
            self.send(reqwest::Method::$method, &$path, $body).await
        })*
    };
}

impl PayoutCoordinator {
    pub fn new(base_url: &str, token: &str) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            token: token.to_string(),
            client: rpc_timeout::rpc_client(),
        }
    }

    async fn send<T: DeserializeOwned>(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<T, CoordinatorError> {
        let mut req = self
            .client
            .request(method, format!("{}{}", self.base_url, path))
            .bearer_auth(&self.token);
        if let Some(b) = body {
            req = req.json(&b);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| CoordinatorError::Unreachable(e.to_string()))?;
        let status = resp.status();
        let text = read_limited(resp, MAX_RESPONSE_BYTES).await?;
        if !status.is_success() {
            let message = serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|v| v.get("error").and_then(Value::as_str).map(str::to_string))
                .unwrap_or(text);
            return Err(CoordinatorError::Refused {
                status: status.as_u16(),
                message,
            });
        }
        serde_json::from_str(&text).map_err(|e| CoordinatorError::Decode(e.to_string()))
    }

    // Expired ids are final: the coordinator refuses to take those transactions again.
    routes! {
        config() -> CoordinatorConfig: GET "/payouts/config", None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// Serves one HTTP response with a body of `len` bytes and returns its URL.
    async fn serve_once(len: usize) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 1024];
            let _ = socket.read(&mut request).await;
            let head =
                format!("HTTP/1.1 200 OK\r\ncontent-length: {len}\r\nconnection: close\r\n\r\n");
            let _ = socket.write_all(head.as_bytes()).await;
            let _ = socket.write_all(&vec![b'a'; len]).await;
        });
        url
    }

    #[tokio::test]
    async fn an_answer_over_the_budget_is_not_read_into_memory() {
        let small = reqwest::get(serve_once(1_000).await).await.unwrap();
        assert_eq!(read_limited(small, 4_096).await.unwrap().len(), 1_000);
        let big = reqwest::get(serve_once(1_000_000).await).await.unwrap();
        let err = read_limited(big, 4_096).await.unwrap_err();
        assert!(matches!(err, CoordinatorError::Decode(_)), "{err:?}");
    }
}
