//! One error type for the whole API, and how each case looks to the caller.
//!
//! Callers never see database or gateway internals: those are logged here and
//! the caller gets a short, safe message.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    /// The caller sent something we cannot accept (400).
    #[error("bad request: {0}")]
    BadRequest(String),
    /// The thing asked for does not exist for this tenant (404).
    #[error("not found")]
    NotFound,
    /// A signature or credential did not check out (401).
    #[error("unauthorised")]
    Unauthorised,
    /// Too many attempts in a short time (429).
    #[error("rate limited")]
    RateLimited,
    /// The caller is known but may not do this (403).
    #[error("forbidden")]
    Forbidden,
    /// The staff account has not set up 2-step verification yet (403, code `mfa_required`).
    #[error("2-step verification required")]
    MfaRequired,
    /// The payment gateway failed or answered nonsense (502).
    #[error("payment gateway error: {0}")]
    Gateway(String),
    /// Anything else. Logged in full, hidden from the caller (500).
    #[error("internal error: {0}")]
    Internal(String),
}

impl From<sqlx::Error> for AppError {
    fn from(e: sqlx::Error) -> Self {
        AppError::Internal(format!("database: {e}"))
    }
}

/// Attached to a failed response so the error-log middleware can record what really
/// happened (the caller only sees a safe message).
#[derive(Debug, Clone)]
pub struct ErrorDetail {
    pub source: &'static str,
    pub code: &'static str,
    pub message: String,
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let detail = match &self {
            AppError::Gateway(m) => Some(ErrorDetail { source: "payment", code: "payment.gateway", message: m.clone() }),
            AppError::Internal(m) if m.starts_with("database") => Some(ErrorDetail { source: "database", code: "database.query", message: m.clone() }),
            AppError::Internal(m) => Some(ErrorDetail { source: "api", code: "api.internal", message: m.clone() }),
            _ => None,
        };
        let (status, message) = match &self {
            AppError::MfaRequired => (StatusCode::FORBIDDEN, "set up 2-step verification before using the back office".to_string()),
            AppError::BadRequest(m) => (StatusCode::BAD_REQUEST, m.clone()),
            AppError::NotFound => (StatusCode::NOT_FOUND, "not found".to_string()),
            AppError::Unauthorised => (StatusCode::UNAUTHORIZED, "unauthorised".to_string()),
            AppError::RateLimited => (StatusCode::TOO_MANY_REQUESTS, "too many attempts, wait a few minutes and try again".to_string()),
            AppError::Forbidden => (StatusCode::FORBIDDEN, "you do not have permission to do that".to_string()),
            AppError::Gateway(_) => (StatusCode::BAD_GATEWAY, "payment provider unavailable, try again".to_string()),
            AppError::Internal(_) => (StatusCode::INTERNAL_SERVER_ERROR, "something went wrong".to_string()),
        };
        if matches!(self, AppError::Gateway(_) | AppError::Internal(_)) {
            tracing::error!(error = %self, "request failed");
        }
        let mut body = json!({ "error": message });
        if matches!(self, AppError::MfaRequired) {
            body["code"] = json!("mfa_required");
        }
        let mut res = (status, Json(body)).into_response();
        if let Some(d) = detail {
            res.extensions_mut().insert(d);
        }
        res
    }
}

pub type Result<T> = std::result::Result<T, AppError>;
