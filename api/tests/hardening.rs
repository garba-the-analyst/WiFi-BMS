//! Headers, request ids, CORS and size limits. Needs no database.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use platform_api::harden::{harden, parse_origins};
use platform_api::http::router;
use platform_api::{auth::RateLimiter, secrets::MasterKey, AppState, Config};
use sqlx::postgres::PgPoolOptions;
use std::sync::Arc;
use tower::ServiceExt;

fn app() -> axum::Router {
    let pool = PgPoolOptions::new().connect_lazy("postgres://nobody@127.0.0.1:1/none").unwrap();
    let state = AppState {
        pool,
        gateway: Arc::new(platform_api::paystack::PaystackHttp::new()),
        master_key: MasterKey::from_hex(&"00".repeat(32)).unwrap(),
        config: Config { public_base_url: "https://w.test".into(), fallback_email_domain: "w.test".into(), trust_proxy: false, require_2fa: false },
        limiter: Arc::new(RateLimiter::new()),
        sms: Arc::new(platform_api::sms::DisabledSms),
        mailer: Arc::new(platform_api::mail::MemoryMailer::default()),
    };
    harden(router(state), parse_origins("https://portal.example.com"))
}

#[tokio::test]
async fn every_response_carries_the_security_headers_and_a_request_id() {
    let r = app().oneshot(Request::get("/healthz").body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let h = r.headers();
    assert_eq!(h["x-content-type-options"], "nosniff");
    assert_eq!(h["x-frame-options"], "DENY");
    assert_eq!(h["cache-control"], "no-store");
    assert!(h["content-security-policy"].to_str().unwrap().contains("default-src 'none'"));
    assert!(!h["x-request-id"].is_empty());
}

#[tokio::test]
async fn an_error_response_also_has_headers_and_a_unique_request_id() {
    let a = app().oneshot(Request::get("/v1/t/x/admin/overview").body(Body::empty()).unwrap()).await.unwrap();
    let b = app().oneshot(Request::get("/v1/t/x/admin/overview").body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(a.headers()["x-content-type-options"], "nosniff");
    assert_ne!(a.headers()["x-request-id"], b.headers()["x-request-id"]);
}

#[tokio::test]
async fn only_listed_origins_get_cors_permission() {
    let ok = app().oneshot(Request::get("/healthz").header("origin", "https://portal.example.com").body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(ok.headers()["access-control-allow-origin"], "https://portal.example.com");
    let bad = app().oneshot(Request::get("/healthz").header("origin", "https://evil.example").body(Body::empty()).unwrap()).await.unwrap();
    assert!(bad.headers().get("access-control-allow-origin").is_none());
}

#[tokio::test]
async fn oversized_bodies_are_refused_before_processing() {
    let big = "x".repeat(200 * 1024);
    let r = app().oneshot(Request::post("/v1/t/x/members/login").header("content-type", "application/json").body(Body::from(big)).unwrap()).await.unwrap();
    assert_eq!(r.status(), StatusCode::PAYLOAD_TOO_LARGE);
}
