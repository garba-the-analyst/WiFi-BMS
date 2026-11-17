//! Admin API against a real PostgreSQL with row-level security on.
//! See `run_integration.sh`. Skipped (passes) without the environment variables.

use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use platform_api::auth::{sha256_hex, RateLimiter};
use platform_api::error::{AppError, Result};
use platform_api::http::router;
use platform_api::paystack::{Gateway, InitializeRequest, InitializedTransaction, VerifiedTransaction};
use platform_api::secrets::MasterKey;
use platform_api::{totp, AppState, Config};
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

struct NoGateway;
#[async_trait]
impl Gateway for NoGateway {
    async fn initialize(&self, _: InitializeRequest<'_>) -> Result<InitializedTransaction> {
        Err(AppError::Gateway("unused".into()))
    }
    async fn verify(&self, _: &str, _: &str) -> Result<VerifiedTransaction> {
        Err(AppError::Gateway("unused".into()))
    }
}

struct World {
    admin: PgPool,
    state: AppState,
    slug: String,
    tenant: Uuid,
    site_id: Uuid,
    profile: Uuid,
}

async fn world() -> Option<World> {
    let (Ok(app_url), Ok(admin_url)) = (std::env::var("DATABASE_URL"), std::env::var("ADMIN_DATABASE_URL")) else {
        eprintln!("NOTICE: DATABASE_URL / ADMIN_DATABASE_URL not set; skipping integration test");
        return None;
    };
    let app = PgPoolOptions::new().max_connections(5).connect(&app_url).await.unwrap();
    let admin = PgPoolOptions::new().max_connections(2).connect(&admin_url).await.unwrap();
    let slug = format!("a-{}", &Uuid::new_v4().simple().to_string()[..10]);
    let tenant: Uuid = sqlx::query_scalar("INSERT INTO tenants (name, slug) VALUES ($1, $1) RETURNING id").bind(&slug).fetch_one(&admin).await.unwrap();
    let site_id: Uuid = sqlx::query_scalar("INSERT INTO sites (tenant_id, name, slug) VALUES ($1, 'Site 1', 'site-1') RETURNING id")
        .bind(tenant)
        .fetch_one(&admin)
        .await
        .unwrap();
    let profile: Uuid = sqlx::query_scalar("INSERT INTO speed_profiles (tenant_id, name, down_kbps, up_kbps) VALUES ($1, 'Std', 5000, 2000) RETURNING id")
        .bind(tenant)
        .fetch_one(&admin)
        .await
        .unwrap();
    let state = AppState {
        pool: app,
        gateway: Arc::new(NoGateway),
        master_key: MasterKey::from_hex(&"00".repeat(32)).unwrap(),
        config: Config { public_base_url: "https://wifi.test".into(), fallback_email_domain: "guest.test".into(), trust_proxy: false },
        limiter: Arc::new(RateLimiter::new()),
        sms: Arc::new(platform_api::sms::LogSms),
    };
    Some(World { admin, state, slug, tenant, site_id, profile })
}

impl World {
    async fn call(&self, method: &str, path: &str, token: Option<&str>, body: Option<Value>) -> (StatusCode, Value, String) {
        let mut b = Request::builder().method(method).uri(format!("/v1/t/{}/admin{}", self.slug, path)).header("content-type", "application/json");
        if let Some(t) = token {
            b = b.header("authorization", format!("Bearer {t}"));
        }
        let resp = router(self.state.clone()).oneshot(b.body(body.map(|v| Body::from(v.to_string())).unwrap_or_else(Body::empty)).unwrap()).await.unwrap();
        let st = resp.status();
        let text = String::from_utf8(resp.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap();
        (st, serde_json::from_str(&text).unwrap_or(Value::Null), text)
    }

    async fn admin(&self, email: &str, role: &str) -> String {
        platform_api::admin::create_admin(&self.state, &self.slug, email, "correct horse 42", role).await.unwrap();
        let (s, v, _) = self.call("POST", "/login", None, Some(json!({ "email": email, "password": "correct horse 42" }))).await;
        assert_eq!(s, StatusCode::OK, "{v}");
        v["token"].as_str().unwrap().to_string()
    }
}

fn plan_body(w: &World, name: &str) -> Value {
    json!({ "name": name, "kind": "time", "price_kobo": 20000, "active_seconds": 7200, "valid_seconds": 86400, "speed_profile_id": w.profile })
}

#[tokio::test]
async fn signing_in_and_the_role_ladder() {
    let Some(w) = world().await else { return };
    let owner = w.admin("owner@x.test", "owner").await;
    let viewer = w.admin("viewer@x.test", "viewer").await;
    let operator = w.admin("op@x.test", "operator").await;
    assert_eq!(w.call("GET", "/overview", None, None).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(w.call("GET", "/overview", Some("mt_membertoken"), None).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(w.call("GET", "/overview?range=7d", Some(&viewer), None).await.0, StatusCode::OK);
    // viewer cannot write, operator can edit plans but not secrets or tokens, owner can do all.
    assert_eq!(w.call("POST", "/plans", Some(&viewer), Some(plan_body(&w, "P1"))).await.0, StatusCode::FORBIDDEN);
    assert_eq!(w.call("POST", "/plans", Some(&operator), Some(plan_body(&w, "P1"))).await.0, StatusCode::OK);
    assert_eq!(w.call("PUT", "/secrets/paystack", Some(&operator), Some(json!({ "secret": "sk_test_abcdefghijklmnop" }))).await.0, StatusCode::FORBIDDEN);
    assert_eq!(w.call("POST", "/node-tokens", Some(&operator), Some(json!({ "site": "site-1", "name": "x" }))).await.0, StatusCode::FORBIDDEN);
    assert_eq!(w.call("PUT", "/secrets/paystack", Some(&owner), Some(json!({ "secret": "sk_test_abcdefghijklmnop" }))).await.0, StatusCode::OK);
    let (s, _, _) = w.call("PUT", "/secrets/paystack", Some(&owner), Some(json!({ "secret": "not a key" }))).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    // The key is stored encrypted, not as typed.
    let stored: String =
        sqlx::query_scalar("SELECT paystack_secret_enc FROM tenant_secrets WHERE tenant_id = $1").bind(w.tenant).fetch_one(&w.admin).await.unwrap();
    assert!(!stored.contains("sk_test"));
    // Logout kills the token.
    w.call("POST", "/logout", Some(&viewer), None).await;
    assert_eq!(w.call("GET", "/overview", Some(&viewer), None).await.0, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn wrong_passwords_lock_an_admin_and_an_unknown_email_looks_the_same() {
    let Some(w) = world().await else { return };
    platform_api::admin::create_admin(&w.state, &w.slug, "lock@x.test", "correct horse 42", "owner").await.unwrap();
    for _ in 0..5 {
        let (s, _, _) = w.call("POST", "/login", None, Some(json!({ "email": "lock@x.test", "password": "nope nope 123" }))).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED);
    }
    let (s, _, _) = w.call("POST", "/login", None, Some(json!({ "email": "lock@x.test", "password": "correct horse 42" }))).await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
    let (s, _, _) = w.call("POST", "/login", None, Some(json!({ "email": "ghost@x.test", "password": "whatever 123" }))).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn two_step_verification_is_enforced_once_enrolled() {
    let Some(w) = world().await else { return };
    let token = w.admin("2fa@x.test", "owner").await;
    let (s, setup, _) = w.call("POST", "/totp/setup", Some(&token), None).await;
    assert_eq!(s, StatusCode::OK);
    let secret = totp::base32_decode(setup["secret"].as_str().unwrap()).unwrap();
    assert!(setup["otpauth_uri"].as_str().unwrap().starts_with("otpauth://totp/"));
    let now = || std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    // A wrong code does not enable it.
    assert_eq!(w.call("POST", "/totp/confirm", Some(&token), Some(json!({ "code": "000000" }))).await.0, StatusCode::BAD_REQUEST);
    let (s, _, _) = w.call("POST", "/login", None, Some(json!({ "email": "2fa@x.test", "password": "correct horse 42" }))).await;
    assert_eq!(s, StatusCode::OK, "not enabled yet, so no second step");
    assert_eq!(w.call("POST", "/totp/confirm", Some(&token), Some(json!({ "code": totp::code_at(&secret, now(), 6) }))).await.0, StatusCode::OK);
    // Now the password alone is not enough.
    let (s, _, _) = w.call("POST", "/login", None, Some(json!({ "email": "2fa@x.test", "password": "correct horse 42" }))).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, _, _) = w.call("POST", "/login", None, Some(json!({ "email": "2fa@x.test", "password": "correct horse 42", "totp": "123456" }))).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, v, _) =
        w.call("POST", "/login", None, Some(json!({ "email": "2fa@x.test", "password": "correct horse 42", "totp": totp::code_at(&secret, now(), 6) }))).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["totp_enrolled"], true);
}

#[tokio::test]
async fn plans_are_validated_by_the_engine_and_edits_do_not_touch_sold_plans() {
    let Some(w) = world().await else { return };
    let t = w.admin("plans@x.test", "operator").await;
    // A time plan with no time budget is refused (nothing is saved).
    let mut bad = plan_body(&w, "Broken");
    bad["active_seconds"] = Value::Null;
    let (s, _, _) = w.call("POST", "/plans", Some(&t), Some(bad)).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM plans WHERE tenant_id = $1 AND name = 'Broken'").bind(w.tenant).fetch_one(&w.admin).await.unwrap();
    assert_eq!(n, 0, "a rejected plan leaves no row");
    let (s, v, _) = w.call("POST", "/plans", Some(&t), Some(plan_body(&w, "Two hours"))).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let id = v["id"].as_str().unwrap().to_string();
    assert_eq!(w.call("POST", "/plans", Some(&t), Some(plan_body(&w, "Two hours"))).await.0, StatusCode::BAD_REQUEST, "duplicate name");
    let mut upd = plan_body(&w, "Two hours");
    upd["price_kobo"] = json!(25000);
    assert_eq!(w.call("PUT", &format!("/plans/{id}"), Some(&t), Some(upd)).await.0, StatusCode::OK);
    assert_eq!(w.call("PUT", &format!("/plans/{id}/active"), Some(&t), Some(json!({ "active": false }))).await.0, StatusCode::OK);
    let (_, list, _) = w.call("GET", "/plans", Some(&t), None).await;
    assert_eq!(list[0]["price_kobo"], 25000);
    assert_eq!(list[0]["is_active"], false);
    // Another tenant's speed profile cannot be used.
    let other: Uuid = sqlx::query_scalar("INSERT INTO tenants (name, slug) VALUES ('o','o-'||substr(gen_random_uuid()::text,1,8)) RETURNING id")
        .fetch_one(&w.admin)
        .await
        .unwrap();
    let foreign: Uuid = sqlx::query_scalar("INSERT INTO speed_profiles (tenant_id, name, down_kbps, up_kbps) VALUES ($1,'F',1000,1000) RETURNING id")
        .bind(other)
        .fetch_one(&w.admin)
        .await
        .unwrap();
    let mut cross = plan_body(&w, "Cross");
    cross["speed_profile_id"] = json!(foreign);
    let (s, _, _) = w.call("POST", "/plans", Some(&t), Some(cross)).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "a foreign profile must not be usable");
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM plans WHERE name = 'Cross'").fetch_one(&w.admin).await.unwrap();
    assert_eq!(n, 0);
}

#[tokio::test]
async fn analytics_customers_and_network_reports_answer() {
    let Some(w) = world().await else { return };
    let t = w.admin("rep@x.test", "viewer").await;
    let acct: Uuid = sqlx::query_scalar("INSERT INTO accounts (tenant_id, phone, display_name) VALUES ($1, '+2348022223333', 'Amina') RETURNING id")
        .bind(w.tenant)
        .fetch_one(&w.admin)
        .await
        .unwrap();
    sqlx::query("INSERT INTO wallet_ledger (tenant_id, account_id, kind, amount_kobo) VALUES ($1,$2,'topup',50000)")
        .bind(w.tenant)
        .bind(acct)
        .execute(&w.admin)
        .await
        .unwrap();
    for what in ["revenue", "traffic", "devices", "plans"] {
        for range in ["24h", "7d", "30d", "90d"] {
            let (s, v, _) = w.call("GET", &format!("/analytics/{what}?range={range}"), Some(&t), None).await;
            assert_eq!(s, StatusCode::OK, "{what} {range}: {v}");
            assert!(v.is_array());
        }
    }
    assert_eq!(w.call("GET", "/analytics/revenue?range=1y", Some(&t), None).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(w.call("GET", "/analytics/nonsense", Some(&t), None).await.0, StatusCode::NOT_FOUND);
    let (_, ov, _) = w.call("GET", "/overview?range=30d", Some(&t), None).await;
    assert_eq!(ov["wallet_float_kobo"], 50000);
    let (_, list, _) = w.call("GET", "/customers?q=2223333", Some(&t), None).await;
    assert_eq!(list[0]["display_name"], "Amina");
    assert_eq!(list[0]["balance_kobo"], 50000);
    let (s, d, _) = w.call("GET", &format!("/customers/{acct}"), Some(&t), None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(d["account"]["phone"], "+2348022223333");
    assert_eq!(w.call("GET", &format!("/customers/{}", Uuid::new_v4()), Some(&t), None).await.0, StatusCode::NOT_FOUND);
    // Wildcards in a search are not wildcards.
    let (_, none, _) = w.call("GET", "/customers?q=%25", Some(&t), None).await;
    assert!(none.as_array().unwrap().len() <= 1);
    assert_eq!(w.call("GET", "/network", Some(&t), None).await.0, StatusCode::OK);
    assert_eq!(w.call("GET", "/security", Some(&t), None).await.0, StatusCode::OK);
}

#[tokio::test]
async fn node_tokens_work_once_issued_and_stop_when_revoked_and_metrics_are_stored() {
    let Some(w) = world().await else { return };
    let owner = w.admin("tok@x.test", "owner").await;
    let (s, v, _) = w.call("POST", "/node-tokens", Some(&owner), Some(json!({ "site": "site-1", "name": "Site 1 PC" }))).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let token = v["token"].as_str().unwrap().to_string();
    let id = v["id"].as_str().unwrap().to_string();
    let leaked: i64 = sqlx::query_scalar("SELECT count(*) FROM node_tokens WHERE token_hash = $1").bind(&token).fetch_one(&w.admin).await.unwrap();
    assert_eq!(leaked, 0, "only the hash is stored");
    let stored: i64 = sqlx::query_scalar("SELECT count(*) FROM node_tokens WHERE token_hash = $1").bind(sha256_hex(&token)).fetch_one(&w.admin).await.unwrap();
    assert_eq!(stored, 1);

    let send = |tok: String| {
        let st = w.state.clone();
        async move {
            let req = Request::post("/v1/node/metrics").header("content-type", "application/json").header("authorization", format!("Bearer {tok}"))
                .body(Body::from(json!({ "metrics": [{ "metric": "agent_up", "value": 1.0 }, { "metric": "agent_spool_depth", "value": 3.0 }, { "metric": "bad name!", "value": 1.0 }] }).to_string())).unwrap();
            let r = router(st).oneshot(req).await.unwrap();
            let s = r.status();
            (s, serde_json::from_slice::<Value>(&r.into_body().collect().await.unwrap().to_bytes()).unwrap_or(Value::Null))
        }
    };
    let (s, v) = send(token.clone()).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["stored"], 2, "the badly named metric is dropped");
    let (_, net, _) = w.call("GET", "/network", Some(&owner), None).await;
    let site = net["sites"].as_array().unwrap().iter().find(|s| s["slug"] == "site-1").unwrap();
    assert_eq!(site["spool_depth"], 3.0);
    assert!(site["agent_last_seen"].is_string());

    assert_eq!(w.call("POST", &format!("/node-tokens/{id}/revoke"), Some(&owner), None).await.0, StatusCode::OK);
    assert_eq!(send(token).await.0, StatusCode::UNAUTHORIZED);
    let _ = w.site_id;
}

#[tokio::test]
async fn the_audit_trail_records_admin_actions_verifies_and_exports_safely() {
    let Some(w) = world().await else { return };
    let owner = w.admin("aud@x.test", "owner").await;
    w.call("POST", "/plans", Some(&owner), Some(plan_body(&w, "=cmd|' /C calc'!A0"))).await;
    let (_, entries, _) = w.call("GET", "/audit?action=plan.", Some(&owner), None).await;
    assert_eq!(entries[0]["action"], "plan.created");
    assert_eq!(entries[0]["actor_kind"], "admin");
    let (s, v, _) = w.call("GET", "/audit/verify", Some(&owner), None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["intact"], true);
    let (s, _, csv) = w.call("GET", "/audit.csv", Some(&owner), None).await;
    assert_eq!(s, StatusCode::OK);
    assert!(csv.starts_with("seq,ts,actor_kind"));
    assert!(!csv.contains(",\"=cmd"), "no cell may start with '='");
    // Tampering (done as the database owner, which the app role cannot do) is caught.
    let seq: i64 = sqlx::query_scalar("SELECT min(seq) FROM audit_log WHERE tenant_id = $1").bind(w.tenant).fetch_one(&w.admin).await.unwrap();
    sqlx::query("ALTER TABLE audit_log DISABLE TRIGGER audit_append_only").execute(&w.admin).await.unwrap();
    sqlx::query("UPDATE audit_log SET action = 'forged' WHERE seq = $1").bind(seq).execute(&w.admin).await.unwrap();
    sqlx::query("ALTER TABLE audit_log ENABLE TRIGGER audit_append_only").execute(&w.admin).await.unwrap();
    let (_, v, _) = w.call("GET", "/audit/verify", Some(&owner), None).await;
    assert_eq!(v["intact"], false);
}

#[tokio::test]
async fn one_operators_admin_token_means_nothing_in_another_tenant() {
    let Some(w) = world().await else { return };
    let t = w.admin("x@x.test", "owner").await;
    let other_slug = format!("z-{}", &Uuid::new_v4().simple().to_string()[..8]);
    sqlx::query("INSERT INTO tenants (name, slug) VALUES ($1, $1)").bind(&other_slug).execute(&w.admin).await.unwrap();
    let req = Request::get(format!("/v1/t/{other_slug}/admin/overview")).header("authorization", format!("Bearer {t}")).body(Body::empty()).unwrap();
    let resp = router(w.state.clone()).oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}
