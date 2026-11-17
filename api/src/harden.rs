//! Cross-cutting protections applied to the whole router.
//!
//! * Security headers on every response (this API only speaks JSON and CSV,
//!   so the content-security-policy forbids everything).
//! * `Cache-Control: no-store`: tokens, balances and audit data must never be cached.
//! * A request id on every request and response, and in the log line, so an
//!   operator can match a customer's complaint to the server's record.
//! * A 30-second ceiling per request, so a stuck upstream cannot pile up work.
//! * CORS limited to the origins you list (the portal and the admin app). With
//!   no origins configured, browsers on other sites are refused.

use std::time::Duration;

use axum::http::{header, HeaderName, HeaderValue, Method};
use axum::Router;
use tower_http::cors::CorsLayer;
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};
use tower_http::set_header::SetResponseHeaderLayer;
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::TraceLayer;

/// Parse `CORS_ORIGINS` (comma separated). Anything that is not a plain
/// `https://host` (or `http://localhost...` for development) is ignored.
pub fn parse_origins(list: &str) -> Vec<HeaderValue> {
    list.split(',')
        .map(str::trim)
        .filter(|o| o.starts_with("https://") || o.starts_with("http://localhost"))
        .filter(|o| !o.ends_with('/') && !o.contains('*'))
        .filter_map(|o| HeaderValue::from_str(o).ok())
        .collect()
}

pub fn harden(router: Router, origins: Vec<HeaderValue>) -> Router {
    let rid = HeaderName::from_static("x-request-id");
    let cors = CorsLayer::new()
        .allow_origin(origins)
        .allow_methods([Method::GET, Method::POST, Method::PUT, Method::OPTIONS])
        .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE])
        .max_age(Duration::from_secs(600));
    let fixed = |name: &'static str, value: &'static str| SetResponseHeaderLayer::overriding(HeaderName::from_static(name), HeaderValue::from_static(value));
    router
        .layer(TimeoutLayer::with_status_code(axum::http::StatusCode::REQUEST_TIMEOUT, Duration::from_secs(30)))
        .layer(cors)
        .layer(fixed("x-content-type-options", "nosniff"))
        .layer(fixed("x-frame-options", "DENY"))
        .layer(fixed("referrer-policy", "no-referrer"))
        .layer(fixed("cache-control", "no-store"))
        .layer(fixed("content-security-policy", "default-src 'none'; frame-ancestors 'none'"))
        .layer(fixed("permissions-policy", "geolocation=(), camera=(), microphone=()"))
        .layer(TraceLayer::new_for_http())
        .layer(PropagateRequestIdLayer::new(rid.clone()))
        .layer(SetRequestIdLayer::new(rid, MakeRequestUuid))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_plain_https_origins_are_accepted() {
        let o = parse_origins("https://wifi.example.com, http://localhost:5173, http://evil.com, https://*.example.com, https://a.com/");
        let o: Vec<_> = o.iter().map(|v| v.to_str().unwrap()).collect();
        assert_eq!(o, vec!["https://wifi.example.com", "http://localhost:5173"]);
    }
}
