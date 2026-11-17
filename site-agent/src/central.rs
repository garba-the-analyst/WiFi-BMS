//! Talking to the central platform.

use std::time::Duration;

use async_trait::async_trait;

use crate::wire::{AccountingReply, AccountingReq, AuthorizeReply, AuthorizeReq};

#[derive(Debug, thiserror::Error)]
pub enum CentralError {
    /// Network down, timeout, TLS failure or a 5xx: worth retrying later.
    #[error("central platform unreachable: {0}")]
    Unreachable(String),
    /// The platform answered but refused the request (4xx): retrying will not help.
    #[error("central platform rejected the request ({0})")]
    Rejected(u16),
}

impl CentralError {
    /// Should the caller queue the request and try again later?
    pub fn is_retryable(&self) -> bool {
        matches!(self, CentralError::Unreachable(_))
    }
}

#[async_trait]
pub trait Central: Send + Sync {
    async fn authorize(&self, req: &AuthorizeReq) -> Result<AuthorizeReply, CentralError>;
    async fn accounting(&self, req: &AccountingReq) -> Result<AccountingReply, CentralError>;
    /// Health numbers for the admin Network screen. Best effort.
    async fn metrics(&self, metrics: &[(String, f64)]) -> Result<(), CentralError>;
}

pub struct HttpCentral {
    client: reqwest::Client,
    base: String,
    token: String,
}

impl HttpCentral {
    pub fn new(base: &str, token: &str) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(8))
            .connect_timeout(Duration::from_secs(4))
            .build()
            .expect("HTTP client builds");
        HttpCentral { client, base: base.trim_end_matches('/').to_string(), token: token.to_string() }
    }

    async fn post<B: serde::Serialize, R: serde::de::DeserializeOwned>(&self, path: &str, body: &B) -> Result<R, CentralError> {
        let resp = self
            .client
            .post(format!("{}{}", self.base, path))
            .bearer_auth(&self.token)
            .json(body)
            .send()
            .await
            .map_err(|e| CentralError::Unreachable(e.to_string()))?;
        let status = resp.status();
        if status.is_server_error() {
            return Err(CentralError::Unreachable(format!("HTTP {status}")));
        }
        if !status.is_success() {
            return Err(CentralError::Rejected(status.as_u16()));
        }
        resp.json().await.map_err(|e| CentralError::Unreachable(format!("bad reply: {e}")))
    }
}

#[async_trait]
impl Central for HttpCentral {
    async fn authorize(&self, req: &AuthorizeReq) -> Result<AuthorizeReply, CentralError> {
        self.post("/v1/node/radius/authorize", req).await
    }

    async fn accounting(&self, req: &AccountingReq) -> Result<AccountingReply, CentralError> {
        self.post("/v1/node/radius/accounting", req).await
    }

    async fn metrics(&self, metrics: &[(String, f64)]) -> Result<(), CentralError> {
        let body = serde_json::json!({ "metrics": metrics.iter().map(|(m, v)| serde_json::json!({ "metric": m, "value": v })).collect::<Vec<_>>() });
        let _: serde_json::Value = self.post("/v1/node/metrics", &body).await?;
        Ok(())
    }
}
