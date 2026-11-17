//! The local HTTP interface FreeRADIUS (`rlm_rest`) talks to.
//!
//! | Path                | Meaning                                              |
//! |---------------------|------------------------------------------------------|
//! | `POST /radius/authorize`  | 200 + reply attributes = accept. 401 = reject. 503 = platform unreachable. |
//! | `POST /radius/accounting` | always 200 once parsed; offline events are queued. |
//! | `GET  /healthz`           | for monitoring |

use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};

use crate::radius_map::{reply_json, to_accounting, to_authorize};
use crate::service::{Agent, Decision};

#[derive(Clone)]
pub struct AppState {
    pub agent: Arc<Agent>,
    pub local_key: Arc<String>,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/radius/authorize", post(authorize))
        .route("/radius/accounting", post(accounting))
        .layer(axum::extract::DefaultBodyLimit::max(32 * 1024))
        .with_state(state)
}

/// Constant-time comparison so the key cannot be learned by timing.
fn key_ok(headers: &HeaderMap, expected: &str) -> bool {
    let given = headers.get("x-agent-key").and_then(|v| v.to_str().ok()).unwrap_or("");
    let (a, b) = (given.as_bytes(), expected.as_bytes());
    let mut diff = (a.len() ^ b.len()) as u8;
    for i in 0..a.len().min(b.len()) {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

async fn authorize(State(st): State<AppState>, headers: HeaderMap, Json(body): Json<Value>) -> (StatusCode, Json<Value>) {
    if !key_ok(&headers, &st.local_key) {
        return (StatusCode::UNAUTHORIZED, Json(json!({ "error": "bad agent key" })));
    }
    let Some(req) = to_authorize(&body) else {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "missing User-Name, User-Password or Calling-Station-Id" })));
    };
    match st.agent.authorize(req).await {
        Decision::Accept(attrs) => (StatusCode::OK, Json(reply_json(&attrs))),
        Decision::Reject(reason) => (StatusCode::UNAUTHORIZED, Json(json!({ "reply:Reply-Message": { "value": [reason], "op": ":=" } }))),
        Decision::Unavailable => {
            (StatusCode::SERVICE_UNAVAILABLE, Json(json!({ "reply:Reply-Message": { "value": ["service temporarily unavailable"], "op": ":=" } })))
        }
    }
}

async fn accounting(State(st): State<AppState>, headers: HeaderMap, Json(body): Json<Value>) -> (StatusCode, Json<Value>) {
    if !key_ok(&headers, &st.local_key) {
        return (StatusCode::UNAUTHORIZED, Json(json!({ "error": "bad agent key" })));
    }
    match to_accounting(&body) {
        Some(req) => {
            st.agent.accounting(req).await;
            (StatusCode::OK, Json(json!({})))
        }
        // Accounting-On/Off and other non-session packets: acknowledge, do nothing.
        None => (StatusCode::OK, Json(json!({}))),
    }
}
